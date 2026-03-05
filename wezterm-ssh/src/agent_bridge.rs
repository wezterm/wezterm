//! Adapt a Windows agent's named pipe to the Unix socket expected by libssh.
//!
//! OpenSSH and Pageant's OpenSSH-compatible pipe both carry a byte stream. The
//! bridge deliberately does not interpret agent messages or cache identities.

use anyhow::{Context, Result};
use smol::channel::{bounded, Sender};
use smol::io::{AsyncReadExt, AsyncWriteExt};
use smol::Async;
use socket2::{Domain, SockAddr, Socket, Type};
use std::io;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

pub struct AgentBridge {
    shutdown: Sender<()>,
    thread: Option<JoinHandle<()>>,
    // Keep the directory until the listener and all connections have closed.
    _directory: tempfile::TempDir,
}

impl Drop for AgentBridge {
    fn drop(&mut self) {
        self.shutdown.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn is_named_pipe(path: &str) -> bool {
    path.replace('/', "\\")
        .get(..9)
        .map(|prefix| prefix.eq_ignore_ascii_case(r"\\.\pipe\"))
        .unwrap_or(false)
}

pub fn create_agent_bridge(pipe_name: PathBuf) -> Result<(PathBuf, AgentBridge)> {
    let pipe_name = PathBuf::from(pipe_name.to_string_lossy().replace('/', "\\"));
    // A PID alone is insufficient: one process can have several SSH sessions.
    // Never unlink another session's socket, either at startup or on shutdown.
    let directory = tempfile::Builder::new()
        .prefix("wezterm-agent-")
        .tempdir()?;
    let socket_path = directory.path().join("agent.sock");
    let listener = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    listener.bind(&SockAddr::unix(&socket_path)?)?;
    listener.listen(128)?;
    let listener = Async::new(listener)?;

    // Tokio supplies cancellable overlapped named-pipe I/O on Windows. smol's
    // socket reactor handles AF_UNIX, which Tokio does not expose on Windows.
    // One thread serves all connections belonging to this SSH session.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    let (shutdown, receiver) = bounded::<()>(1);
    let thread = std::thread::Builder::new()
        .name("ssh-agent-bridge".to_string())
        .spawn(move || {
            runtime.block_on(smol::future::race(
                async {
                    if let Err(err) = run_bridge_server(listener, pipe_name).await {
                        log::error!("SSH agent bridge: {err:#}");
                    }
                },
                async {
                    let _ = receiver.recv().await;
                },
            ));
            // Dropping the runtime cancels pending pipe operations and closes
            // every connection, including idle or unresponsive agents.
        })
        .context("starting SSH agent bridge")?;

    Ok((
        socket_path,
        AgentBridge {
            shutdown,
            thread: Some(thread),
            _directory: directory,
        },
    ))
}

async fn run_bridge_server(listener: Async<Socket>, pipe_name: PathBuf) -> Result<()> {
    loop {
        let (socket, _) = listener.read_with(|s| s.accept()).await?;
        let pipe_name = pipe_name.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_bridge_connection(socket, &pipe_name).await {
                log::debug!("SSH agent bridge to {}: {err:#}", pipe_name.display());
            }
        });
    }
}

async fn connect_pipe(pipe_name: &Path) -> io::Result<NamedPipeClient> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match ClientOptions::new().open(pipe_name) {
            Ok(pipe) => return Ok(pipe),
            // An agent can briefly have no available pipe instances. Do not
            // block other connections (or the SSH session) while waiting.
            Err(err)
                if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                    && Instant::now() < deadline =>
            {
                smol::Timer::after(Duration::from_millis(10)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn handle_bridge_connection(socket: Socket, pipe_name: &Path) -> Result<()> {
    let stream = Async::new(socket)?;
    // Open a fresh pipe for every forwarded channel. A failed connection must
    // not poison later ssh-add invocations or survive an agent restart.
    let pipe = connect_pipe(pipe_name)
        .await
        .context("connecting to agent pipe")?;

    // Drive both directions independently, with bounded buffers and backpressure.
    // A read must never wait for a complete protocol message before the opposite
    // direction can run. On EOF/error, cancel the other direction as well.
    smol::future::race(
        socket_to_pipe(&stream, &pipe),
        pipe_to_socket(&pipe, &stream),
    )
    .await
    .context("forwarding agent stream")
}

async fn socket_to_pipe(mut stream: &Async<Socket>, pipe: &NamedPipeClient) -> io::Result<()> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        let mut remaining = &buf[..n];
        while !remaining.is_empty() {
            pipe.writable().await?;
            match pipe.try_write(remaining) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => remaining = &remaining[n..],
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

async fn pipe_to_socket(pipe: &NamedPipeClient, mut stream: &Async<Socket>) -> io::Result<()> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        pipe.readable().await?;
        match pipe.try_read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => stream.write_all(&buf[..n]).await?,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests;
