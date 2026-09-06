use crate::tmux::{
    classify_handshake_sequence, HandshakeMatch, RefTmuxRemotePane, TmuxCmdQueue, TmuxDomainState,
};
use crate::tmux_commands::{Resize, SendKeys};
use crate::DomainId;
use filedescriptor::FileDescriptor;
use parking_lot::{Condvar, Mutex};
use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty};
use std::io::{Read, Write};
use std::sync::Arc;
use termwiz::tmux_cc::TmuxPaneId;

fn queue_send_keys(
    domain_id: DomainId,
    master_pane: &RefTmuxRemotePane,
    cmd_queue: &Arc<Mutex<TmuxCmdQueue>>,
    buf: &[u8],
) -> std::io::Result<usize> {
    let pane_id = {
        let pane_lock = master_pane.lock();
        pane_lock.pane_id
    };

    if maybe_queue_tmux_report(domain_id, pane_id, cmd_queue, buf) {
        return Ok(buf.len());
    }

    log::trace!("pane:{}, content:{:?}", &pane_id, buf);
    let mut queue = cmd_queue.lock();
    queue.push_back(Box::new(SendKeys {
        pane: pane_id,
        keys: buf.to_vec(),
    }));
    TmuxDomainState::schedule_send_next_command(domain_id);
    Ok(0)
}

fn maybe_queue_tmux_report(
    domain_id: DomainId,
    pane_id: TmuxPaneId,
    cmd_queue: &Arc<Mutex<TmuxCmdQueue>>,
    buf: &[u8],
) -> bool {
    match classify_handshake_sequence(buf) {
        HandshakeMatch::Complete(len) if len == buf.len() && is_handshake_response(buf) => {
            log::trace!(
                "queuing tmux control report for pane {} with payload {:?}",
                pane_id,
                buf
            );
            let mut queue = cmd_queue.lock();
            queue.push_back(Box::new(crate::tmux_commands::SendPaneReport {
                pane: pane_id,
                report: buf.to_vec(),
            }));
            TmuxDomainState::schedule_send_next_command(domain_id);
            true
        }
        _ => false,
    }
}

fn is_handshake_response(buf: &[u8]) -> bool {
    if buf.len() < 2 || buf[0] != 0x1b {
        return false;
    }
    match buf[1] {
        b']' => osc_is_response(buf),
        b'[' => csi_is_response(buf),
        _ => false,
    }
}

fn osc_is_response(buf: &[u8]) -> bool {
    let body = match extract_osc_body(buf) {
        Some(body) => body,
        None => return false,
    };

    let is_color_query = body.starts_with(b"10;") || body.starts_with(b"11;") || body.starts_with(b"12;");
    if !is_color_query {
        return false;
    }

    let after_semicolon = match body.iter().position(|&b| b == b';') {
        Some(idx) if idx + 1 < body.len() => &body[idx + 1..],
        _ => return false,
    };

    match after_semicolon.first() {
        Some(b'?') | None => false,
        _ => true,
    }
}

fn extract_osc_body(buf: &[u8]) -> Option<&[u8]> {
    if buf.len() < 3 || buf[0] != 0x1b || buf[1] != b']' {
        return None;
    }
    let mut idx = 2;
    while idx < buf.len() {
        match buf[idx] {
            0x07 => return Some(&buf[2..idx]),
            0x1b if idx + 1 < buf.len() && buf[idx + 1] == b'\\' => return Some(&buf[2..idx]),
            _ => idx += 1,
        }
    }
    None
}

fn csi_is_response(buf: &[u8]) -> bool {
    if buf.len() < 3 || buf[0] != 0x1b || buf[1] != b'[' {
        return false;
    }
    let data = &buf[2..];
    if data.is_empty() {
        return false;
    }
    let final_byte = *data.last().unwrap();
    let params = &data[..data.len() - 1];

    match final_byte {
        b'c' | b't' => params.contains(&b';'),
        _ => false,
    }
}

/// A local tmux pane(tab) based on a tmux pty
#[derive(Debug)]
pub(crate) struct TmuxPty {
    pub domain_id: DomainId,
    pub master_pane: RefTmuxRemotePane,
    pub reader: FileDescriptor,
    pub cmd_queue: Arc<Mutex<TmuxCmdQueue>>,
}

struct TmuxPtyWriter {
    domain_id: DomainId,
    master_pane: RefTmuxRemotePane,
    cmd_queue: Arc<Mutex<TmuxCmdQueue>>,
}

impl Write for TmuxPtyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        queue_send_keys(self.domain_id, &self.master_pane, &self.cmd_queue, buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Write for TmuxPty {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        queue_send_keys(self.domain_id, &self.master_pane, &self.cmd_queue, buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TmuxChild {
    pub active_lock: Arc<(Mutex<bool>, Condvar)>,
}

impl Child for TmuxChild {
    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        todo!()
    }

    fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        let &(ref lock, ref var) = &*self.active_lock;
        let mut released = lock.lock();
        while !*released {
            var.wait(&mut released);
        }
        return Ok(ExitStatus::with_exit_code(0));
    }

    fn process_id(&self) -> Option<u32> {
        None
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

#[derive(Clone, Debug)]
struct TmuxChildKiller {}

impl ChildKiller for TmuxChildKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "TmuxChildKiller: kill not implemented!",
        ))
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

impl ChildKiller for TmuxChild {
    fn kill(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "TmuxPty: kill not implemented!",
        ))
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(TmuxChildKiller {})
    }
}

impl MasterPty for TmuxPty {
    fn resize(&self, size: portable_pty::PtySize) -> Result<(), anyhow::Error> {
        let mut cmd_queue = self.cmd_queue.lock();
        cmd_queue.push_back(Box::new(Resize {
            size,
            pane_id: self.master_pane.lock().pane_id,
        }));
        TmuxDomainState::schedule_send_next_command(self.domain_id);
        Ok(())
    }

    fn get_size(&self) -> Result<portable_pty::PtySize, anyhow::Error> {
        let pane = self.master_pane.lock();
        Ok(portable_pty::PtySize {
            rows: pane.pane_height as u16,
            cols: pane.pane_width as u16,
            pixel_width: 0,
            pixel_height: 0,
        })
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(self.reader.try_clone()?))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(TmuxPtyWriter {
            domain_id: self.domain_id,
            master_pane: self.master_pane.clone(),
            cmd_queue: self.cmd_queue.clone(),
        }))
    }

    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<libc::pid_t> {
        return None;
    }

    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }

    #[cfg(unix)]
    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}
