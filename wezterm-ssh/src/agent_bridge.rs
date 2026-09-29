//! Bridge between Unix domain sockets and Windows named pipes for SSH agent
//!
//! This module provides functionality to create a Unix domain socket that forwards
//! requests to the Windows SSH agent via named pipes.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const MAX_AGENT_MESSAGE_SIZE: usize = 256 * 1024;

/// Creates a Unix socket bridge to the Windows SSH agent
/// Returns the path to the Unix socket that can be used as SSH_AUTH_SOCK
#[cfg(windows)]
pub fn create_agent_bridge(pipe_name: PathBuf) -> Result<(PathBuf, AgentBridge)> {
    use std::fs;
    use wezterm_uds::UnixListener;

    // Create a temporary directory for the socket
    let temp_dir = std::env::temp_dir();
    let socket_path = temp_dir.join(format!("wezterm-ssh-agent-{}.sock", std::process::id()));

    // Remove old socket if it exists
    let _ = fs::remove_file(&socket_path);

    // Create Unix domain socket listener
    let listener = UnixListener::bind(&socket_path)
        .context("Failed to create Unix domain socket for agent bridge")?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    let socket_path_clone = socket_path.clone();

    // Spawn thread to handle connections
    std::thread::spawn(move || {
        if let Err(e) = run_bridge_server(listener, running_clone, pipe_name) {
            log::error!("Agent bridge server error: {:#}", e);
        }
        // Clean up socket on exit
        let _ = fs::remove_file(&socket_path_clone);
    });

    Ok((socket_path, AgentBridge { running }))
}

#[cfg(not(windows))]
pub fn create_agent_bridge(_pipe_name: PathBuf) -> Result<(PathBuf, AgentBridge)> {
    anyhow::bail!("Agent bridge is only supported on Windows");
}

pub struct AgentBridge {
    running: Arc<AtomicBool>,
}

impl Drop for AgentBridge {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

#[cfg(windows)]
fn run_bridge_server(
    listener: wezterm_uds::UnixListener,
    running: Arc<AtomicBool>,
    pipe_name: PathBuf,
) -> Result<()> {
    use std::time::Duration;

    listener.set_nonblocking(true)
        .context("Failed to set listener to non-blocking")?;

    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let pipe_name = pipe_name.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_bridge_connection(stream, pipe_name) {
                        log::error!("Error handling bridge connection: {:#}", e);
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                log::error!("Error accepting connection: {:#}", e);
                break;
            }
        }
    }

    Ok(())
}

#[cfg(windows)]
fn handle_bridge_connection(
    mut unix_stream: wezterm_uds::UnixStream,
    pipe_name: PathBuf,
) -> Result<()> {
    use std::fs::OpenOptions;

    let mut pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pipe_name)
        .with_context(|| {
            format!(
                "Failed to connect to Windows SSH agent named pipe {}",
                pipe_name.display()
            )
        })?;

    unix_stream
        .set_nonblocking(false)
        .context("Failed to make Unix socket blocking")?;

    // The agent protocol is length-prefixed over a stream. We must buffer
    // until a full request arrives before waiting for the corresponding
    // response from the Windows agent, otherwise larger requests can deadlock
    // if the stream splits them across multiple reads.
    let mut pending = Vec::with_capacity(4096);
    let mut read_buf = [0u8; 4096];

    loop {
        let n = unix_stream
            .read(&mut read_buf)
            .context("Failed to read from Unix socket")?;
        if n == 0 {
            break;
        }

        pending.extend_from_slice(&read_buf[..n]);

        while let Some(request) = take_complete_agent_message(&mut pending)? {
            pipe.write_all(&request)
                .context("Failed to write to named pipe")?;

            let response = read_agent_message(&mut pipe)
                .context("Failed to read response from named pipe")?;
            unix_stream
                .write_all(&response)
                .context("Failed to write to Unix socket")?;
            unix_stream.flush()?;
        }
    }

    Ok(())
}

fn take_complete_agent_message(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    let total_len = match agent_message_len(buf)? {
        Some(total_len) => total_len,
        None => return Ok(None),
    };

    Ok(Some(buf.drain(..total_len).collect()))
}

fn read_agent_message<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;

    let payload_len = parse_agent_payload_len(&header)?;
    let mut message = vec![0u8; 4 + payload_len];
    message[..4].copy_from_slice(&header);
    reader.read_exact(&mut message[4..])?;
    Ok(message)
}

fn agent_message_len(buf: &[u8]) -> Result<Option<usize>> {
    if buf.len() < 4 {
        return Ok(None);
    }

    let payload_len = parse_agent_payload_len(&buf[..4])?;
    let total_len = 4 + payload_len;
    if buf.len() < total_len {
        return Ok(None);
    }

    Ok(Some(total_len))
}

fn parse_agent_payload_len(header: &[u8]) -> Result<usize> {
    let payload_len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if payload_len > MAX_AGENT_MESSAGE_SIZE {
        bail!(
            "SSH agent message size {} exceeds the {} byte safety limit",
            payload_len,
            MAX_AGENT_MESSAGE_SIZE
        );
    }
    Ok(payload_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn buffers_partial_messages_until_complete() {
        let mut pending = vec![0, 0, 0, 5, 1, 2, 3];
        assert!(take_complete_agent_message(&mut pending).unwrap().is_none());

        pending.extend_from_slice(&[4, 5]);
        assert_eq!(
            take_complete_agent_message(&mut pending).unwrap(),
            Some(vec![0, 0, 0, 5, 1, 2, 3, 4, 5])
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn leaves_follow_on_messages_buffered() {
        let mut pending = vec![0, 0, 0, 1, 9, 0, 0, 0, 2, 7, 8];

        assert_eq!(
            take_complete_agent_message(&mut pending).unwrap(),
            Some(vec![0, 0, 0, 1, 9])
        );
        assert_eq!(pending, vec![0, 0, 0, 2, 7, 8]);
        assert_eq!(
            take_complete_agent_message(&mut pending).unwrap(),
            Some(vec![0, 0, 0, 2, 7, 8])
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn reads_length_prefixed_messages() {
        let mut cursor = Cursor::new(vec![0, 0, 0, 3, 0xaa, 0xbb, 0xcc]);
        assert_eq!(
            read_agent_message(&mut cursor).unwrap(),
            vec![0, 0, 0, 3, 0xaa, 0xbb, 0xcc]
        );
    }

    #[test]
    fn rejects_oversized_messages() {
        let err =
            take_complete_agent_message(&mut vec![0, 4, 0, 1]).expect_err("expected error");
        assert!(err
            .to_string()
            .contains("SSH agent message size 262145 exceeds"));
    }
}
