use crate::sessionhandler::{PduSender, SessionHandler};
use anyhow::Context;
use async_ossl::AsyncSslStream;
use codec::{Connection, DecodedPdu, Event, Pdu, Stalled, STALL_LIMIT};
use mux::pane::PaneId;
use mux::{Mux, MuxNotification};
use smol::Async;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use wezterm_uds::UnixStream;

#[cfg(unix)]
pub trait AsRawDesc: std::os::unix::io::AsRawFd + std::os::fd::AsFd {}
#[cfg(windows)]
pub trait AsRawDesc: std::os::windows::io::AsRawSocket + std::os::windows::io::AsSocket {}

impl AsRawDesc for UnixStream {}
impl AsRawDesc for AsyncSslStream {}

/// What waits to be sent to one client: replies and notifications in the
/// order they came, and the panes whose changes it has not been sent yet,
/// each once however often it changed. A pane's changes are worked out only
/// when the client has taken everything sent before, so a client that reads
/// slowly is sent the latest state, never a backlog, and one that stops
/// reading costs nothing more.
#[derive(Default)]
struct Outbox {
    pdus: VecDeque<DecodedPdu>,
    notifications: VecDeque<MuxNotification>,
    changed_panes: VecDeque<PaneId>,
    changed: HashSet<PaneId>,
    /// The session has ended: nothing more is kept for it.
    closed: bool,
}

impl Outbox {
    fn pane_changed(&mut self, pane_id: PaneId) {
        if self.changed.insert(pane_id) {
            self.changed_panes.push_back(pane_id);
        }
    }

    fn next_changed_pane(&mut self) -> Option<PaneId> {
        let pane_id = self.changed_panes.pop_front()?;
        self.changed.remove(&pane_id);
        Some(pane_id)
    }
}

/// An outbox shared with the mux and the session's replies, and the bell that
/// tells the session something was put in it.
#[derive(Clone)]
struct Mailbox {
    outbox: Arc<Mutex<Outbox>>,
    bell: smol::channel::Sender<()>,
}

impl Mailbox {
    /// Put something in the outbox with `f`; false, and nothing put, once
    /// the session has ended.
    fn put(&self, f: impl FnOnce(&mut Outbox)) -> bool {
        {
            let mut outbox = self.outbox.lock().unwrap();
            if outbox.closed {
                return false;
            }
            f(&mut outbox);
        }
        // A full bell has been rung already.
        self.bell.try_send(()).ok();
        true
    }

    fn close(&self) {
        *self.outbox.lock().unwrap() = Outbox {
            closed: true,
            ..Outbox::default()
        };
    }
}

pub async fn process<T>(stream: T) -> anyhow::Result<()>
where
    T: 'static,
    T: std::io::Read,
    T: std::io::Write,
    T: AsRawDesc,
    T: std::fmt::Debug,
    T: async_io::IoSafe,
{
    let stream = smol::Async::new(stream)?;
    process_async(stream).await
}

/// Serve one client: read its requests while writing what waits for it,
/// until it goes away, or takes nothing for `STALL_LIMIT` and is dropped.
pub async fn process_async<T>(stream: Async<T>) -> anyhow::Result<()>
where
    T: 'static,
    T: std::io::Read,
    T: std::io::Write,
    T: std::fmt::Debug,
    T: async_io::IoSafe,
{
    log::trace!("process_async called");

    let (bell, rung) = smol::channel::bounded::<()>(1);
    let mailbox = Mailbox {
        outbox: Arc::new(Mutex::new(Outbox::default())),
        bell,
    };

    let mut handler = SessionHandler::new(PduSender::new({
        let mailbox = mailbox.clone();
        move |pdu| {
            if mailbox.put(|outbox| outbox.pdus.push_back(pdu)) {
                Ok(())
            } else {
                Err(anyhow::anyhow!("the client is gone"))
            }
        }
    }));

    {
        // The session stays subscribed until it ends: its next notification
        // after that unsubscribes it.
        let mailbox = mailbox.clone();
        Mux::get().subscribe(move |n| {
            mailbox.put(|outbox| match n {
                MuxNotification::PaneOutput(pane_id) => outbox.pane_changed(pane_id),
                n => outbox.notifications.push_back(n),
            })
        });
    }

    let mut connection = Connection::new(stream, STALL_LIMIT);
    let result = serve(&mut connection, &mut handler, &mailbox, &rung).await;
    mailbox.close();
    match result {
        Ok(()) => Ok(()),
        Err(err) if client_went_away(&err) => {
            log::debug!("{} went away: {:#}", handler.client_name(), err);
            Ok(())
        }
        Err(err) if err.root_cause().is::<Stalled>() => {
            log::error!("dropping {}: {:#}", handler.client_name(), err);
            Ok(())
        }
        Err(err) => Err(err),
    }
}

async fn serve<S>(
    connection: &mut Connection<S>,
    handler: &mut SessionHandler,
    mailbox: &Mailbox,
    rung: &smol::channel::Receiver<()>,
) -> anyhow::Result<()>
where
    S: smol::io::AsyncRead + smol::io::AsyncWrite + Unpin,
{
    loop {
        let (pdus, notifications) = {
            let mut outbox = mailbox.outbox.lock().unwrap();
            (
                std::mem::take(&mut outbox.pdus),
                std::mem::take(&mut outbox.notifications),
            )
        };
        for decoded in pdus {
            connection.send(&decoded.pdu, decoded.serial)?;
        }
        for notification in notifications {
            if let Some(pdu) = notification_pdu(notification, handler, mailbox) {
                connection.send(&pdu, 0)?;
            }
        }
        if connection.pending() == 0 {
            let changed = mailbox.outbox.lock().unwrap().next_changed_pane();
            if let Some(pane_id) = changed {
                // Its changes go into the outbox, to be sent next time round.
                handler.push_pane_changes(pane_id)?;
                continue;
            }
        }
        match connection.next(rung.recv()).await? {
            Event::Pdu(decoded) => handler.process_one(*decoded),
            Event::Drained => {}
            Event::Other(rang) => {
                rang.context("the session's bell closed while the session runs")?;
            }
        }
    }
}

/// The PDU that tells the client of `notification`, if one does; a pane's
/// output and alerts go with the pane's next changes instead.
fn notification_pdu(
    notification: MuxNotification,
    handler: &mut SessionHandler,
    mailbox: &Mailbox,
) -> Option<Pdu> {
    match notification {
        MuxNotification::PaneOutput(pane_id) => {
            mailbox.outbox.lock().unwrap().pane_changed(pane_id);
            None
        }
        MuxNotification::Alert { pane_id, alert } => {
            handler
                .per_pane(pane_id)
                .lock()
                .unwrap()
                .notifications
                .push(alert);
            mailbox.outbox.lock().unwrap().pane_changed(pane_id);
            None
        }
        MuxNotification::PaneRemoved(pane_id) => {
            Some(Pdu::PaneRemoved(codec::PaneRemoved { pane_id }))
        }
        MuxNotification::AssignClipboard {
            pane_id,
            selection,
            clipboard,
        } => Some(Pdu::SetClipboard(codec::SetClipboard {
            pane_id,
            clipboard,
            selection,
        })),
        MuxNotification::TabAddedToWindow { tab_id, window_id } => {
            Some(Pdu::TabAddedToWindow(codec::TabAddedToWindow {
                tab_id,
                window_id,
            }))
        }
        MuxNotification::WindowWorkspaceChanged(window_id) => {
            let workspace = Mux::get()
                .get_window(window_id)
                .map(|w| w.get_workspace().to_string())?;
            Some(Pdu::WindowWorkspaceChanged(codec::WindowWorkspaceChanged {
                window_id,
                workspace,
            }))
        }
        MuxNotification::PaneFocused(pane_id) => {
            Some(Pdu::PaneFocused(codec::PaneFocused { pane_id }))
        }
        MuxNotification::TabResized(tab_id) => Some(Pdu::TabResized(codec::TabResized { tab_id })),
        MuxNotification::TabTitleChanged { tab_id, title } => {
            Some(Pdu::TabTitleChanged(codec::TabTitleChanged {
                tab_id,
                title,
            }))
        }
        MuxNotification::WindowTitleChanged { window_id, title } => {
            Some(Pdu::WindowTitleChanged(codec::WindowTitleChanged {
                window_id,
                title,
            }))
        }
        MuxNotification::WorkspaceRenamed {
            old_workspace,
            new_workspace,
        } => Some(Pdu::RenameWorkspace(codec::RenameWorkspace {
            old_workspace,
            new_workspace,
        })),
        MuxNotification::PaneAdded(_)
        | MuxNotification::SaveToDownloads { .. }
        | MuxNotification::WindowRemoved(_)
        | MuxNotification::WindowCreated(_)
        | MuxNotification::WindowInvalidated(_)
        | MuxNotification::ActiveWorkspaceChanged(_)
        | MuxNotification::Empty => None,
    }
}

/// Whether `err` says the client closed its end, which is how a client
/// leaves.
fn client_went_away(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|err| {
            matches!(
                err.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
            )
        })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_pane_that_changes_again_before_it_is_sent_is_sent_once_in_its_first_place() {
        let mut outbox = Outbox::default();
        for pane_id in [3, 1, 3, 2, 1, 3] {
            outbox.pane_changed(pane_id);
        }
        let mut sent = vec![];
        while let Some(pane_id) = outbox.next_changed_pane() {
            sent.push(pane_id);
        }
        assert_eq!(sent, vec![3, 1, 2]);

        // Once sent, a change queues the pane again.
        outbox.pane_changed(1);
        assert_eq!(outbox.next_changed_pane(), Some(1));
        assert_eq!(outbox.next_changed_pane(), None);
    }

    #[test]
    fn a_closed_mailbox_keeps_nothing() {
        let (bell, rung) = smol::channel::bounded::<()>(1);
        let mailbox = Mailbox {
            outbox: Arc::new(Mutex::new(Outbox::default())),
            bell,
        };
        assert!(mailbox.put(|outbox| outbox.pane_changed(1)));
        assert!(rung.try_recv().is_ok());

        mailbox.close();
        assert!(!mailbox.put(|outbox| outbox.pane_changed(2)));
        assert!(rung.try_recv().is_err());
        assert_eq!(mailbox.outbox.lock().unwrap().next_changed_pane(), None);
    }

    /// A client that writes all of its requests before it reads any reply,
    /// far more of both than the socket holds either way, is read and
    /// answered: the server reads while its replies wait to be written.
    #[cfg(unix)]
    #[test]
    fn a_client_sending_while_its_replies_wait_is_read_and_answered() {
        use std::io::{BufReader, Write};
        use std::time::Duration;

        const PINGS: u64 = 300_000;
        const WAIT: Duration = Duration::from_secs(20);

        let mux = Arc::new(Mux::new(None));
        Mux::set_mux(&mux);

        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            smol::block_on(process_async(Async::new(server).unwrap())).ok();
        });

        let mut requests = vec![];
        for serial in 1..=PINGS {
            Pdu::Ping(codec::Ping {})
                .encode(&mut requests, serial)
                .unwrap();
        }
        let mut writer = client.try_clone().unwrap();
        let (sent, written) = std::sync::mpsc::channel();
        std::thread::spawn(move || sent.send(writer.write_all(&requests)));
        written
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| {
                panic!(
                    "the server stopped reading the client's requests while its replies \
                     waited: {} pings were not taken in {:?}",
                    PINGS, WAIT
                )
            })
            .unwrap();

        client.set_read_timeout(Some(WAIT)).unwrap();
        let mut replies = BufReader::new(client);
        for serial in 1..=PINGS {
            let DecodedPdu { serial: got, pdu } = Pdu::decode(&mut replies)
                .unwrap_or_else(|err| panic!("reply {} of {}: {:#}", serial, PINGS, err));
            assert_eq!((got, pdu.pdu_name()), (serial, "Pong"));
        }
    }
}
