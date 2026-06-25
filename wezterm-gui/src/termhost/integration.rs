//! GUI-integration callback for the termhost COM handoff.
//!
//! Runs on the COM apartment thread; we do the minimum work necessary
//! here (pipe allocation, out-param population, handle wrapping) and
//! dispatch the mux-attaching work onto the WezTerm executor.

use std::mem;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Context;
use portable_pty::{Child, MasterPty, PtySize};
use winapi::shared::ntdef::HANDLE;

use wezterm_term::TerminalSize;

use super::{
    create_anon_pipe, pid_of, RawHandlesMasterPty, TermHostChild, TerminalStartupInfoOwned,
    HANDOFF_RECEIVED,
};

/// Per `ITerminalHandoff3` (IDL lines 75-76), `in` and `out` are
/// `[out] HANDLE*` — WezTerm allocates the ConPTY pipes and writes the
/// ConPTY-side ends through these pointers, keeping the terminal-side
/// ends. Conhost takes ownership of the handles we hand back (extracts
/// via `wil::unique_handle::release`, closes on session end).
pub(crate) fn handle_handoff(
    in_handle_out: *mut HANDLE,
    out_handle_out: *mut HANDLE,
    signal: HANDLE,
    reference: HANDLE,
    _server: HANDLE,
    client: HANDLE,
    startup: TerminalStartupInfoOwned,
) -> anyhow::Result<()> {
    let client_pid = pid_of(client);
    HANDOFF_RECEIVED.store(true, Ordering::SeqCst);

    debug_assert!(
        !in_handle_out.is_null() && !out_handle_out.is_null(),
        "IDL contract: ITerminalHandoff3 [out] HANDLE* must be non-null"
    );

    let (their_read_in, our_write) =
        create_anon_pipe(true, false).context("create_anon_pipe for ConPTY stdin")?;
    let (our_read, their_write_out) =
        create_anon_pipe(false, true).context("create_anon_pipe for ConPTY stdout")?;

    // Forget the ConPTY-side OwnedHandles: their handles are now owned
    // by the caller's out-param slots. If we let them drop, they would
    // close before the COM runtime marshals them to conhost.
    let their_read_raw = their_read_in.as_raw_handle() as HANDLE;
    let their_write_raw = their_write_out.as_raw_handle() as HANDLE;
    mem::forget(their_read_in);
    mem::forget(their_write_out);

    unsafe {
        *in_handle_out = their_read_raw;
        *out_handle_out = their_write_raw;
    }

    let initial_size = PtySize {
        rows: if startup.height != 0 {
            startup.height
        } else {
            24
        },
        cols: if startup.width != 0 {
            startup.width
        } else {
            80
        },
        pixel_width: 0,
        pixel_height: 0,
    };

    // Same rationale as above: detach the terminal-side OwnedHandles
    // before from_raw_handles takes ownership (it wraps the read handle
    // and duplicates the write handle).
    let our_read_raw = our_read.as_raw_handle() as HANDLE;
    let our_write_raw = our_write.as_raw_handle() as HANDLE;
    mem::forget(our_read);
    mem::forget(our_write);

    let master: Box<dyn MasterPty + Send> = Box::new(unsafe {
        RawHandlesMasterPty::from_raw_handles(
            our_read_raw,
            our_write_raw,
            signal,
            reference,
            initial_size,
        )
    });
    let child: Box<dyn Child + Send + Sync> =
        Box::new(unsafe { TermHostChild::from_raw(client, client_pid) });

    let title = startup
        .title
        .clone()
        .unwrap_or_else(|| "WezTerm (termhost)".to_string());

    promise::spawn::spawn(async move {
        let _activity = mux::activity::Activity::new();

        if let Err(e) = attach_pane_to_new_window(master, child, title, initial_size).await {
            log::error!("wezterm-gui: termhost attach failed: {e:#}");
        }
    })
    .detach();

    Ok(())
}

async fn attach_pane_to_new_window(
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    title: String,
    initial_size: PtySize,
) -> anyhow::Result<()> {
    // Use `try_get` not `get`: if conhost delivers a handoff between
    // CoRegisterClassObject and Mux::build_initial_mux, `get` would
    // panic. Returning Err propagates to E_FAIL and conhost falls back.
    let mux = mux::Mux::try_get().context("Mux not yet initialized when handoff arrived")?;

    let domain = mux
        .get_domain_by_name("local")
        .context("no 'local' domain registered with the mux")?;
    let local_domain = domain
        .downcast_ref::<mux::domain::LocalDomain>()
        .ok_or_else(|| {
            anyhow::anyhow!("the 'local' domain is not a LocalDomain; termhost cannot attach")
        })?;

    let size = TerminalSize {
        rows: initial_size.rows as usize,
        cols: initial_size.cols as usize,
        ..Default::default()
    };

    let command_description = format!("termhost handoff: {}", title);

    let pane = local_domain
        .attach_external_pane(size, master, child, command_description)
        .context("LocalDomain::attach_external_pane")?;

    let tab = Arc::new(mux::tab::Tab::new(&size));
    tab.assign_pane(&pane);
    mux.add_tab_and_active_pane(&tab)
        .context("add_tab_and_active_pane")?;

    let workspace = mux.active_workspace();
    let builder = mux.new_empty_window(Some(workspace), None);
    mux.add_tab_to_window(&tab, *builder)
        .context("add_tab_to_window")?;

    Ok(())
}
