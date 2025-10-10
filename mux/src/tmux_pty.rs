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

fn queue_send_keys(
    domain_id: DomainId,
    master_pane: &RefTmuxRemotePane,
    cmd_queue: &Arc<Mutex<TmuxCmdQueue>>,
    buf: &[u8],
) -> std::io::Result<usize> {
    if should_suppress_tmux_handshake(buf) {
        log::trace!("suppressing tmux control handshake payload: {:?}", buf);
        return Ok(0);
    }

    let pane_id = {
        let pane_lock = master_pane.lock();
        pane_lock.pane_id
    };
    log::trace!("pane:{}, content:{:?}", &pane_id, buf);
    let mut queue = cmd_queue.lock();
    queue.push_back(Box::new(SendKeys {
        pane: pane_id,
        keys: buf.to_vec(),
    }));
    TmuxDomainState::schedule_send_next_command(domain_id);
    Ok(0)
}

fn should_suppress_tmux_handshake(buf: &[u8]) -> bool {
    matches!(classify_handshake_sequence(buf), HandshakeMatch::Complete(len) if len == buf.len())
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
