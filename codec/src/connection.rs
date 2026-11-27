//! One end of a multiplexer protocol connection, reading and writing at once.
//!
//! Each end of a connection sends whenever it likes: a client sends requests
//! while replies are on their way to it, and a server sends replies and pane
//! changes while requests are on their way to it. An end that stops reading
//! while it waits to write deadlocks with a peer doing the same, once both
//! have more to send than the socket holds: so a `Connection` writes what is
//! queued while it reads, and reads while it writes.
//!
//! A peer that takes nothing of what is waiting for it for the connection's
//! stall limit is taken to be gone: the connection fails with [`Stalled`].

use crate::{frame_len, DecodedPdu, Pdu};
use anyhow::Context as _;
use smol::io::{AsyncRead, AsyncWrite};
use smol::Timer;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// How long an end of a connection waits on a peer that takes nothing it
/// sends before it gives the peer up.
pub const STALL_LIMIT: Duration = Duration::from_secs(30);

/// How much a connection reads at a time.
const READ_SIZE: usize = 64 * 1024;

/// Written bytes are dropped from the front of what is queued once there
/// are this many of them.
const COMPACT_AT: usize = 1024 * 1024;

/// The peer took none of the bytes waiting for it for the stall limit.
#[derive(Debug, thiserror::Error)]
#[error("the peer took none of the {pending} bytes waiting for it for {limit:?}")]
pub struct Stalled {
    pub pending: usize,
    pub limit: Duration,
}

/// What [`Connection::next`] waited for.
#[derive(Debug)]
pub enum Event<T> {
    /// A PDU the peer sent.
    Pdu(Box<DecodedPdu>),
    /// Everything queued was written.
    Drained,
    /// The other future given to `next` completed.
    Other(T),
}

pub struct Connection<S> {
    stream: S,
    /// Encoded PDUs; those before `written` are on their way.
    outgoing: Vec<u8>,
    written: usize,
    /// Whether the stream has bytes written since it was last flushed.
    unflushed: bool,
    /// Bytes read and not yet decoded.
    incoming: Vec<u8>,
    stall_limit: Duration,
    /// Armed while bytes wait for the peer; moved on whenever it takes some.
    stall: Option<Timer>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    pub fn new(stream: S, stall_limit: Duration) -> Self {
        Self {
            stream,
            outgoing: vec![],
            written: 0,
            unflushed: false,
            incoming: vec![],
            stall_limit,
            stall: None,
        }
    }

    /// Queue `pdu` to be written with `serial`; [`Connection::next`] writes it.
    pub fn send(&mut self, pdu: &Pdu, serial: u64) -> anyhow::Result<()> {
        pdu.encode(&mut self.outgoing, serial)
            .with_context(|| format!("encoding {}", pdu.pdu_name()))
    }

    /// How many queued bytes the peer has not taken yet.
    pub fn pending(&self) -> usize {
        self.outgoing.len() - self.written
    }

    /// Write what is queued, as far as the peer takes it, until the peer
    /// sends a PDU, everything queued is written, or `other` completes. A
    /// PDU the peer sent is handed over before `other` is looked at.
    ///
    /// Fails when the peer closes the connection (an `UnexpectedEof`
    /// `std::io::Error`), when reading or writing fails, and with
    /// [`Stalled`] when the peer takes nothing for the stall limit.
    pub async fn next<F: Future>(&mut self, other: F) -> anyhow::Result<Event<F::Output>> {
        smol::pin!(other);
        let had_pending = self.pending() > 0;
        std::future::poll_fn(|cx| self.poll_next(cx, other.as_mut(), had_pending)).await
    }

    fn poll_next<F: Future>(
        &mut self,
        cx: &mut Context<'_>,
        other: Pin<&mut F>,
        had_pending: bool,
    ) -> Poll<anyhow::Result<Event<F::Output>>> {
        if let Some(pdu) = self.decode()? {
            return Poll::Ready(Ok(Event::Pdu(Box::new(pdu))));
        }
        self.poll_write(cx)?;
        if let Some(pdu) = self.poll_read(cx)? {
            return Poll::Ready(Ok(Event::Pdu(Box::new(pdu))));
        }
        if had_pending && self.pending() == 0 && !self.unflushed {
            return Poll::Ready(Ok(Event::Drained));
        }
        if let Poll::Ready(output) = other.poll(cx) {
            return Poll::Ready(Ok(Event::Other(output)));
        }
        if let Some(stall) = self.stall.as_mut() {
            if Pin::new(stall).poll(cx).is_ready() {
                return Poll::Ready(Err(Stalled {
                    pending: self.pending(),
                    limit: self.stall_limit,
                }
                .into()));
            }
        }
        Poll::Pending
    }

    /// Write as much of what is queued as the peer takes now.
    fn poll_write(&mut self, cx: &mut Context<'_>) -> anyhow::Result<()> {
        let mut took = false;
        while self.written < self.outgoing.len() {
            match Pin::new(&mut self.stream).poll_write(cx, &self.outgoing[self.written..]) {
                Poll::Ready(Ok(0)) => {
                    return Err(std::io::Error::from(std::io::ErrorKind::WriteZero))
                        .context("writing to the peer")
                }
                Poll::Ready(Ok(n)) => {
                    self.written += n;
                    self.unflushed = true;
                    took = true;
                }
                Poll::Ready(Err(err)) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Poll::Ready(Err(err)) => return Err(err).context("writing to the peer"),
                Poll::Pending => break,
            }
        }
        if self.written == self.outgoing.len() {
            self.outgoing.clear();
            self.written = 0;
        } else if self.written >= COMPACT_AT {
            self.outgoing.drain(..self.written);
            self.written = 0;
        }
        if self.unflushed && self.pending() == 0 {
            match Pin::new(&mut self.stream).poll_flush(cx) {
                Poll::Ready(Ok(())) => self.unflushed = false,
                Poll::Ready(Err(err)) => return Err(err).context("flushing to the peer"),
                Poll::Pending => {}
            }
        }
        if self.pending() == 0 && !self.unflushed {
            self.stall = None;
        } else if took || self.stall.is_none() {
            match self.stall.as_mut() {
                Some(stall) => {
                    stall.set_after(self.stall_limit);
                }
                None => self.stall = Some(Timer::after(self.stall_limit)),
            }
        }
        Ok(())
    }

    /// Read what the peer has sent until a whole PDU is in hand.
    fn poll_read(&mut self, cx: &mut Context<'_>) -> anyhow::Result<Option<DecodedPdu>> {
        let mut buf = [0u8; READ_SIZE];
        loop {
            match Pin::new(&mut self.stream).poll_read(cx, &mut buf) {
                Poll::Ready(Ok(0)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the peer closed the connection",
                    )
                    .into())
                }
                Poll::Ready(Ok(n)) => {
                    self.incoming.extend_from_slice(&buf[..n]);
                    if let Some(pdu) = self.decode()? {
                        return Ok(Some(pdu));
                    }
                }
                Poll::Ready(Err(err)) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Poll::Ready(Err(err)) => return Err(err).context("reading from the peer"),
                Poll::Pending => return Ok(None),
            }
        }
    }

    /// The first PDU read whole, taken out of what was read.
    fn decode(&mut self) -> anyhow::Result<Option<DecodedPdu>> {
        let len = match frame_len(&self.incoming)? {
            Some(len) if len <= self.incoming.len() => len,
            _ => return Ok(None),
        };
        let decoded = Pdu::decode(&self.incoming[..len]).context("decoding a PDU")?;
        self.incoming.drain(..len);
        Ok(Some(decoded))
    }
}

#[cfg(all(test, unix))]
mod test {
    use super::*;
    use crate::{Ping, Pong, WriteToPane};
    use smol::Async;
    use std::os::unix::net::UnixStream;

    fn pair() -> (Connection<Async<UnixStream>>, Connection<Async<UnixStream>>) {
        let (a, b) = UnixStream::pair().unwrap();
        (
            Connection::new(Async::new(a).unwrap(), STALL_LIMIT),
            Connection::new(Async::new(b).unwrap(), STALL_LIMIT),
        )
    }

    fn big(serial: u64) -> Pdu {
        // Incompressible, so it is as large on the wire as it is here.
        let mut state = serial.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let data = (0..256 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        Pdu::WriteToPane(WriteToPane { pane_id: 0, data })
    }

    /// Both ends queue far more than the socket holds and then wait for
    /// what the other sent: each must read while it writes.
    #[test]
    fn both_ends_sending_more_than_the_socket_holds_each_get_it_all() {
        const EACH: u64 = 32;
        let (mut left, mut right) = pair();
        for serial in 1..=EACH {
            left.send(&big(serial), serial).unwrap();
            right.send(&big(serial + 1000), serial + 1000).unwrap();
        }
        async fn receive(conn: &mut Connection<Async<UnixStream>>, first: u64) {
            let mut got = vec![];
            while got.len() < EACH as usize {
                match conn.next(std::future::pending::<()>()).await.unwrap() {
                    Event::Pdu(pdu) => got.push(pdu),
                    Event::Drained | Event::Other(()) => {}
                }
            }
            // What is still queued goes out before the end returns.
            while conn.pending() > 0 {
                conn.next(std::future::pending::<()>()).await.unwrap();
            }
            assert_eq!(
                got.iter().map(|pdu| pdu.serial).collect::<Vec<_>>(),
                (first..first + EACH).collect::<Vec<_>>()
            );
            for pdu in got {
                assert_eq!(pdu.pdu, big(pdu.serial));
            }
        }
        let both = smol::future::zip(receive(&mut left, 1001), receive(&mut right, 1));
        smol::block_on(smol::future::or(async { Some(both.await) }, async {
            Timer::after(Duration::from_secs(60)).await;
            None
        }))
        .expect("the two ends deadlocked");
    }

    #[test]
    fn pdus_split_and_joined_arbitrarily_on_the_wire_are_decoded_in_order() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut conn = Connection::new(Async::new(b).unwrap(), STALL_LIMIT);
        let mut wire = vec![];
        for serial in 1..=50u64 {
            Pdu::Ping(Ping {}).encode(&mut wire, serial).unwrap();
            big(serial).encode(&mut wire, serial + 100).unwrap();
            Pdu::Pong(Pong {}).encode(&mut wire, serial + 200).unwrap();
        }
        let writer = std::thread::spawn(move || {
            let mut a = a;
            for chunk in wire.chunks(7_777) {
                std::io::Write::write_all(&mut a, chunk).unwrap();
            }
        });
        let got = smol::block_on(async {
            let mut got = vec![];
            while got.len() < 150 {
                if let Event::Pdu(pdu) = conn.next(std::future::pending::<()>()).await.unwrap() {
                    got.push(pdu);
                }
            }
            got
        });
        writer.join().unwrap();
        let expected: Vec<u64> = (1..=50u64)
            .flat_map(|serial| [serial, serial + 100, serial + 200])
            .collect();
        assert_eq!(
            got.iter().map(|pdu| pdu.serial).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn a_peer_that_closes_ends_the_connection_as_end_of_file() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut conn = Connection::new(Async::new(b).unwrap(), STALL_LIMIT);
        drop(a);
        let err = smol::block_on(conn.next(std::future::pending::<()>())).unwrap_err();
        assert_eq!(
            err.root_cause()
                .downcast_ref::<std::io::Error>()
                .map(|err| err.kind()),
            Some(std::io::ErrorKind::UnexpectedEof)
        );
    }

    #[test]
    fn a_peer_that_takes_nothing_for_the_stall_limit_is_given_up() {
        let (_a, b) = UnixStream::pair().unwrap();
        let limit = Duration::from_millis(300);
        let mut conn = Connection::new(Async::new(b).unwrap(), limit);
        for serial in 1..=16 {
            conn.send(&big(serial), serial).unwrap();
        }
        let started = std::time::Instant::now();
        let err = smol::block_on(conn.next(std::future::pending::<()>())).unwrap_err();
        let stalled = err.downcast_ref::<Stalled>().expect("a Stalled error");
        assert!(stalled.pending > 0);
        assert!(started.elapsed() >= limit);
    }

    #[test]
    fn a_peer_that_keeps_taking_is_never_given_up() {
        let (a, b) = UnixStream::pair().unwrap();
        let limit = Duration::from_millis(300);
        let mut conn = Connection::new(Async::new(b).unwrap(), limit);
        for serial in 1..=4 {
            conn.send(&big(serial), serial).unwrap();
        }
        let queued = conn.pending();
        // The peer reads slowly: a little at a time, never pausing as long
        // as the limit, for several times the limit in all.
        let reader = std::thread::spawn(move || {
            let mut a = a;
            let mut buf = vec![0u8; 64 * 1024];
            let mut total = 0;
            while total < queued {
                std::thread::sleep(Duration::from_millis(100));
                total += std::io::Read::read(&mut a, &mut buf).unwrap();
            }
        });
        let event = smol::block_on(conn.next(std::future::pending::<()>())).unwrap();
        assert!(matches!(event, Event::Drained));
        reader.join().unwrap();
    }
}
