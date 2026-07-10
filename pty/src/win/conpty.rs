use crate::cmdbuilder::CommandBuilder;
use crate::win::psuedocon::PsuedoCon;
use crate::{Child, MasterPty, PtyPair, PtySize, PtySystem, SlavePty};
use anyhow::Error;
use filedescriptor::{FileDescriptor, Pipe};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;
use winapi::um::wincon::COORD;

#[derive(Default)]
pub struct ConPtySystem {}

impl PtySystem for ConPtySystem {
    fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
        let stdin = Pipe::new()?;
        let stdout = Pipe::new()?;

        let con = PsuedoCon::new(
            COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            },
            stdin.read,
            stdout.write,
        )?;

        let master = ConPtyMasterPty {
            inner: Arc::new(Mutex::new(Inner {
                con: Some(con),
                readable: Some(stdout.read),
                writable: Some(stdin.write),
                size,
            })),
        };

        let slave = ConPtySlavePty {
            inner: master.inner.clone(),
        };

        Ok(PtyPair {
            master: Box::new(master),
            slave: Box::new(slave),
        })
    }
}

struct Inner {
    con: Option<PsuedoCon>,
    readable: Option<FileDescriptor>,
    writable: Option<FileDescriptor>,
    size: PtySize,
}

impl Inner {
    pub fn resize(
        &mut self,
        num_rows: u16,
        num_cols: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<(), Error> {
        let con = self
            .con
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("pseudo console is shutting down"))?;
        con.resize(COORD {
            X: num_cols as i16,
            Y: num_rows as i16,
        })?;
        self.size = PtySize {
            rows: num_rows,
            cols: num_cols,
            pixel_width,
            pixel_height,
        };
        Ok(())
    }
}

fn drain_conpty_output(mut output: FileDescriptor) {
    let started = Instant::now();
    let mut bytes_drained = 0u64;
    let mut buffer = [0u8; 64 * 1024];

    loop {
        match output.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => bytes_drained += size as u64,
            Err(err) => {
                log::debug!("conpty teardown output drain ended with: {err:#}");
                break;
            }
        }
    }

    log::warn!(
        "[close-tab-diagnostic] ConPTY output drain end bytes={} elapsed_ms={} thread={:?}",
        bytes_drained,
        started.elapsed().as_millis(),
        thread::current().id()
    );
}

fn schedule_conpty_close(con: PsuedoCon, output: Option<FileDescriptor>) -> anyhow::Result<()> {
    // ClosePseudoConsole may wait for the pseudoconsole to finish writing its
    // final output on Windows versions prior to 11 24H2.  Always move teardown
    // off the caller.  Explicit shutdown also supplies an otherwise-idle copy
    // of the output pipe so that it remains drained during the close.
    let con = con.detach();
    let started = Instant::now();
    let close_thread = thread::Builder::new()
        .name("conpty-close".to_string())
        .spawn(move || {
            let drain_thread = output.map(|output| {
                thread::Builder::new()
                    .name("conpty-output-drain".to_string())
                    .spawn(move || drain_conpty_output(output))
            });

            con.close();

            if let Some(drain_thread) = drain_thread {
                match drain_thread {
                    Ok(drain_thread) => {
                        if drain_thread.join().is_err() {
                            log::error!("ConPTY output drain thread panicked");
                        }
                    }
                    Err(err) => {
                        log::error!("failed to spawn ConPTY output drain thread: {err:#}");
                    }
                }
            }

            log::warn!(
                "[close-tab-diagnostic] ConPTY background teardown end elapsed_ms={} thread={:?}",
                started.elapsed().as_millis(),
                thread::current().id()
            );
        });

    close_thread.map(|_| ()).map_err(|err| {
        // The detached wrapper intentionally has no Drop implementation.
        // Leaking a pseudoconsole handle is preferable to invoking a
        // potentially unbounded blocking call on the caller.  Windows will
        // reclaim it when this process exits.
        anyhow::anyhow!("failed to spawn ConPTY close thread; leaking handle: {err:#}")
    })
}

impl Inner {
    fn shutdown(&mut self) -> anyhow::Result<()> {
        let Some(con) = self.con.take() else {
            return Ok(());
        };

        let output = self.readable.take();
        schedule_conpty_close(con, output)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(con) = self.con.take() {
            // No explicit shutdown took place, so don't add a competing reader
            // that could consume output expected by the caller.  Dropping our
            // normal pipe handle after this method returns will still unblock
            // ClosePseudoConsole when no cloned reader remains.
            if let Err(err) = schedule_conpty_close(con, None) {
                log::error!("{err:#}");
            }
        }
    }
}

#[derive(Clone)]
pub struct ConPtyMasterPty {
    inner: Arc<Mutex<Inner>>,
}

pub struct ConPtySlavePty {
    inner: Arc<Mutex<Inner>>,
}

impl MasterPty for ConPtyMasterPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.resize(size.rows, size.cols, size.pixel_width, size.pixel_height)
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.size.clone())
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
        let inner = self.inner.lock().unwrap();
        let readable = inner
            .readable
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ConPTY output is shutting down"))?;
        Ok(Box::new(readable.try_clone()?))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
        Ok(Box::new(
            self.inner
                .lock()
                .unwrap()
                .writable
                .take()
                .ok_or_else(|| anyhow::anyhow!("writer already taken"))?,
        ))
    }

    fn shutdown(&self) -> anyhow::Result<()> {
        self.inner.lock().unwrap().shutdown()
    }
}

impl SlavePty for ConPtySlavePty {
    fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        let inner = self.inner.lock().unwrap();
        let child = inner
            .con
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("pseudo console is shutting down"))?
            .spawn_command(cmd)?;
        Ok(Box::new(child))
    }
}
