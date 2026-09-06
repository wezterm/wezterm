use crate::activity::Activity;
use crate::domain::{alloc_domain_id, Domain, DomainId, DomainState, SplitSource};
use crate::pane::{Pane, PaneId};
use crate::tab::{SplitRequest, Tab, TabId};
use crate::tmux_commands::{
    ListAllPanes, ListAllWindows, ListCommands, NewWindow, SplitPane, SwapWindow, TmuxCommand,
};
use crate::window::WindowId;
use crate::{Mux, MuxWindowBuilder};
use async_trait::async_trait;
use filedescriptor::FileDescriptor;
use parking_lot::{Condvar, Mutex};
use portable_pty::CommandBuilder;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::sync::Arc;
use termwiz::tmux_cc::*;
use wezterm_term::TerminalSize;

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum AttachState {
    Init,
    Done,
}

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
enum State {
    WaitForInitialGuard,
    Idle,
    WaitingForResponse,
    Exit,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct TmuxRemotePane {
    // members for local
    pub local_pane_id: PaneId,
    pub output_write: FileDescriptor,
    pub active_lock: Arc<(Mutex<bool>, Condvar)>,
    // members sync with remote
    pub session_id: TmuxSessionId,
    pub window_id: TmuxWindowId,
    pub pane_id: TmuxPaneId,
    pub cursor_x: u64,
    pub cursor_y: u64,
    pub pane_width: u64,
    pub pane_height: u64,
    pub pane_left: u64,
    pub pane_top: u64,
}

pub(crate) type RefTmuxRemotePane = Arc<Mutex<TmuxRemotePane>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandshakeMatch {
    Complete(usize),
    Incomplete,
    None,
}

fn osc_body_len(data: &[u8]) -> Option<usize> {
    for (idx, byte) in data.iter().enumerate() {
        match byte {
            0x07 => return Some(idx + 1),
            0x1b if data.get(idx + 1) == Some(&b'\\') => return Some(idx + 2),
            _ => {}
        }
    }
    None
}

fn csi_body_len(data: &[u8]) -> Option<(usize, u8)> {
    for (idx, byte) in data.iter().enumerate() {
        if (0x40..=0x7e).contains(byte) {
            return Some((idx, *byte));
        }
    }
    None
}

fn first_numeric_param(data: &[u8]) -> Option<u16> {
    let mut value = 0u16;
    let mut seen_digit = false;
    for &byte in data {
        if byte == b';' {
            break;
        }
        if byte.is_ascii_digit() {
            seen_digit = true;
            value = value
                .saturating_mul(10)
                .saturating_add((byte - b'0') as u16);
        } else {
            return None;
        }
    }
    seen_digit.then_some(value)
}

fn classify_osc(bytes: &[u8]) -> HandshakeMatch {
    if bytes.len() < 2 || bytes[0] != 0x1b || bytes[1] != b']' {
        return HandshakeMatch::None;
    }
    let body = match bytes.get(2..) {
        Some(body) => body,
        None => return HandshakeMatch::Incomplete,
    };
    if !(body.starts_with(b"10;") || body.starts_with(b"11;") || body.starts_with(b"12;")) {
        return HandshakeMatch::None;
    }
    match osc_body_len(body) {
        Some(len) => HandshakeMatch::Complete(2 + len),
        None => HandshakeMatch::Incomplete,
    }
}

fn is_device_attributes(params: &[u8], final_byte: u8) -> bool {
    final_byte == b'c'
        && params
            .first()
            .map(|b| matches!(*b, b'?' | b'>' | b'='))
            .unwrap_or(false)
}

fn is_geometry_report(params: &[u8], final_byte: u8) -> bool {
    if final_byte != b't' {
        return false;
    }
    matches!(first_numeric_param(params), Some(4 | 8 | 14 | 16))
}

fn classify_csi(bytes: &[u8]) -> HandshakeMatch {
    if bytes.len() < 3 || bytes[0] != 0x1b || bytes[1] != b'[' {
        return HandshakeMatch::None;
    }
    let data = &bytes[2..];
    if data.is_empty() {
        return HandshakeMatch::Incomplete;
    }
    let (body_len, final_byte) = match csi_body_len(data) {
        Some(res) => res,
        None => return HandshakeMatch::Incomplete,
    };
    let params = &data[..body_len];
    if is_device_attributes(params, final_byte) || is_geometry_report(params, final_byte) {
        HandshakeMatch::Complete(2 + body_len + 1)
    } else {
        HandshakeMatch::None
    }
}

fn is_tmux_passthru_dcs(body: &[u8]) -> bool {
    body.starts_with(b"tmux;")
}

fn classify_dcs(bytes: &[u8]) -> HandshakeMatch {
    if bytes.len() < 3 || bytes[0] != 0x1b || bytes[1] != b'P' {
        return HandshakeMatch::None;
    }
    let mut idx = 2;
    while idx < bytes.len() {
        match bytes[idx] {
            0x07 => {
                let body = &bytes[2..idx];
                return if is_tmux_passthru_dcs(body) {
                    HandshakeMatch::Complete(idx + 1)
                } else {
                    HandshakeMatch::None
                };
            }
            0x1b => {
                if idx + 1 >= bytes.len() {
                    return HandshakeMatch::Incomplete;
                }
                if bytes[idx + 1] == b'\\' {
                    let body = &bytes[2..idx];
                    return if is_tmux_passthru_dcs(body) {
                        HandshakeMatch::Complete(idx + 2)
                    } else {
                        HandshakeMatch::None
                    };
                }
                idx += 1;
            }
            _ => idx += 1,
        }
    }
    HandshakeMatch::Incomplete
}

pub(crate) fn classify_handshake_sequence(bytes: &[u8]) -> HandshakeMatch {
    if bytes.len() < 2 || bytes[0] != 0x1b {
        return HandshakeMatch::None;
    }
    match bytes[1] {
        b']' => classify_osc(bytes),
        b'[' => classify_csi(bytes),
        b'P' => classify_dcs(bytes),
        _ => HandshakeMatch::None,
    }
}

/// As a remote TmuxTab, keeping the TmuxPanes ID
/// within the remote tab.
#[allow(dead_code)]
pub(crate) struct TmuxTab {
    pub tab_id: TabId, // local tab ID
    pub tmux_window_id: TmuxWindowId,
    pub layout_csum: String,
    pub title: String,
    pub panes: HashSet<TmuxPaneId>, // tmux panes within tmux window
}

pub(crate) type TmuxCmdQueue = VecDeque<Box<dyn TmuxCommand>>;
pub(crate) struct TmuxDomainState {
    pub pane_id: PaneId,     // ID of the original pane
    pub domain_id: DomainId, // ID of TmuxDomain
    state: Mutex<State>,
    pub cmd_queue: Arc<Mutex<TmuxCmdQueue>>,
    pub gui_window: Mutex<Option<MuxWindowBuilder>>,
    pub gui_tabs: Mutex<HashMap<TmuxWindowId, TmuxTab>>,
    pub remote_panes: Mutex<HashMap<TmuxPaneId, RefTmuxRemotePane>>,
    pub tmux_session: Mutex<Option<TmuxSessionId>>,
    pub support_commands: Mutex<HashMap<String, String>>,
    pub attach_state: Mutex<AttachState>,
    pending_splits: Mutex<VecDeque<promise::Promise<TmuxPaneId>>>,
    pub backlog: Mutex<HashMap<TmuxPaneId, Vec<u8>>>,
    pub handshake_buffers: Mutex<HashMap<TmuxPaneId, Vec<u8>>>,
    pub window_order: Mutex<Vec<TmuxWindowId>>,
}

pub struct TmuxDomain {
    pub(crate) inner: Arc<TmuxDomainState>,
}

impl TmuxDomainState {
    fn strip_handshake_sequences(&self, pane: TmuxPaneId, text: &[u8]) -> Vec<u8> {
        let mut buffers = self.handshake_buffers.lock();
        let output = {
            let buffer = buffers.entry(pane).or_insert_with(Vec::new);
            buffer.extend_from_slice(text);

            let mut output = Vec::new();
            let mut processed = 0usize;
            while processed < buffer.len() {
                match classify_handshake_sequence(&buffer[processed..]) {
                    HandshakeMatch::Complete(len) => {
                        processed += len;
                    }
                    HandshakeMatch::Incomplete => break,
                    HandshakeMatch::None => {
                        output.push(buffer[processed]);
                        processed += 1;
                    }
                }
            }

            if processed > 0 {
                buffer.drain(..processed);
            }
            output
        };

        if buffers
            .get(&pane)
            .map(|buf| buf.is_empty())
            .unwrap_or(false)
        {
            buffers.remove(&pane);
        }

        output
    }

    pub fn reconcile_tab_order(&self, local_window_id: WindowId) {
        if *self.attach_state.lock() != AttachState::Done {
            return;
        }

        let mux = Mux::get();

        let desired: Vec<TmuxWindowId> = {
            let Some(window) = mux.get_window(local_window_id) else {
                return;
            };

            let gui_tabs = self.gui_tabs.lock();
            if gui_tabs.is_empty() {
                return;
            }
            let mut tab_to_window = HashMap::new();
            for (window_id, tab) in gui_tabs.iter() {
                tab_to_window.insert(tab.tab_id, *window_id);
            }
            drop(gui_tabs);

            window
                .iter()
                .filter_map(|tab| tab_to_window.get(&tab.tab_id()).copied())
                .collect()
        };

        if desired.len() <= 1 {
            *self.window_order.lock() = desired;
            return;
        }

        let mut current = {
            let order = self.window_order.lock();
            if order.is_empty() {
                desired.clone()
            } else {
                order.clone()
            }
        };

        if current == desired {
            return;
        }

        if current.len() != desired.len() {
            *self.window_order.lock() = desired;
            return;
        }

        let mut ops = Vec::new();
        for i in 0..desired.len() {
            if current[i] == desired[i] {
                continue;
            }
            if let Some(j) = (i + 1..current.len()).find(|&j| current[j] == desired[i]) {
                let src = current[i];
                let dst = current[j];
                ops.push((src, dst));
                current.swap(i, j);
            } else {
                *self.window_order.lock() = desired;
                return;
            }
        }

        if ops.is_empty() {
            *self.window_order.lock() = desired;
            return;
        }

        {
            let mut queue = self.cmd_queue.lock();
            for (src, dst) in ops {
                queue.push_back(Box::new(SwapWindow { src, dst }));
            }
        }

        TmuxDomainState::schedule_send_next_command(self.domain_id);
        *self.window_order.lock() = desired;
    }

    pub fn advance(&self, events: Box<Vec<Event>>) {
        for event in events.iter() {
            let state = *self.state.lock();
            log::debug!("tmux: {:?} in state {:?}", event, state);
            match event {
                // Tmux generic events
                Event::Guarded(response) => match state {
                    State::WaitForInitialGuard => {
                        *self.state.lock() = State::Idle;
                    }
                    State::WaitingForResponse => {
                        let mut cmd_queue = self.cmd_queue.as_ref().lock();
                        if let Some(cmd) = cmd_queue.pop_front() {
                            let domain_id = self.domain_id;
                            *self.state.lock() = State::Idle;
                            let resp = response.clone();
                            promise::spawn::spawn_into_main_thread(async move {
                                if let Err(err) = cmd.process_result(domain_id, &resp) {
                                    log::error!("Tmux processing command result error: {}", err);
                                }
                            })
                            .detach();
                        }
                    }
                    State::Idle => {}
                    State::Exit => {}
                },

                // Tmux specific events
                Event::ConfigError { error } => {
                    // tmux config file error, not our fault, just log it and go
                    log::warn!("tmux configuration error: {error}");
                }
                Event::Exit { reason: _ } => {
                    *self.state.lock() = State::Exit;
                    let mut pane_map = self.remote_panes.lock();
                    for (_, v) in pane_map.iter_mut() {
                        let remote_pane = v.lock();
                        let (lock, condvar) = &*remote_pane.active_lock;
                        let mut released = lock.lock();
                        *released = true;
                        condvar.notify_all();
                    }
                    let mut cmd_queue = self.cmd_queue.as_ref().lock();
                    cmd_queue.clear();

                    // Force to quit the tmux mode
                    let pane_id = self.pane_id;
                    promise::spawn::spawn_into_main_thread_with_low_priority(async move {
                        if let Some(x) = Mux::get().get_pane(pane_id) {
                            let _ = write!(x.writer(), "\n\n");
                        }
                    })
                    .detach();

                    return;
                }
                Event::LayoutChange {
                    window,
                    layout,
                    visible_layout: _,
                    raw_flags: _,
                } => {
                    let mut cmd_queue = self.cmd_queue.as_ref().lock();
                    cmd_queue.push_back(Box::new(ListAllPanes {
                        window_id: *window,
                        prune: true,
                        layout_csum: if let Some(l) = layout.get(0..4) {
                            l.to_string()
                        } else {
                            "".to_string()
                        },
                    }));
                }
                Event::Output { pane, text } => {
                    let filtered = self.strip_handshake_sequences(*pane, text);
                    if filtered.is_empty() {
                        continue;
                    }

                    let pane_map = self.remote_panes.lock();
                    if let Some(ref_pane) = pane_map.get(pane) {
                        let mut tmux_pane = ref_pane.lock();
                        if let Err(err) = tmux_pane.output_write.write_all(&filtered) {
                            log::error!("Failed to write tmux data to output: {:#}", err);
                        }
                    } else {
                        // the output may come early then pane is ready, in this case we
                        // backlog it
                        self.backlog.lock().insert(*pane, filtered);
                        log::debug!("Tmux pane {} havn't been attached", pane);
                    }
                }
                Event::SessionChanged { session, name: _ } => {
                    *self.tmux_session.lock() = Some(*session);
                    let mut cmd_queue = self.cmd_queue.as_ref().lock();
                    cmd_queue.push_back(Box::new(ListCommands));

                    self.subscribe_notification();
                    log::info!("tmux session changed:{}", session);
                }
                Event::WindowAdd { window } => {
                    // Only handle the new tab, the first empty window handled by sync_window_state
                    if !self.gui_window.lock().is_none() {
                        if let Some(session) = *self.tmux_session.lock() {
                            let mut cmd_queue = self.cmd_queue.as_ref().lock();
                            cmd_queue.push_back(Box::new(ListAllWindows {
                                session_id: session,
                                window_id: Some(*window),
                            }));
                            log::info!("tmux window add: {}:{}", session, window);
                        }
                    }
                }
                Event::WindowClose { window } => {
                    let _ = self.remove_detached_window(*window);
                }
                Event::WindowPaneChanged { window, pane } => {
                    // The tmux 2.7 WindowPaneChanged event comes early than WindowAdd, we need to
                    // skip it
                    if !self.check_window_attached(*window) {
                        continue;
                    }

                    // Split pane
                    if !self.check_pane_attached(*window, *pane) {
                        let mut pending_splits = self.pending_splits.lock();
                        if let Some(mut promise) = pending_splits.pop_front() {
                            promise.ok(*pane);
                        }
                    }
                    log::info!("tmux window pane changed: {}:{}", window, pane);
                }
                Event::WindowRenamed { window, name } => {
                    let title = format!("{}", name);
                    let tab_id = {
                        let mut gui_tabs = self.gui_tabs.lock();
                        gui_tabs.get_mut(&window).map(|tab| {
                            tab.title = title.clone();
                            tab.tab_id
                        })
                    };
                    if let Some(tab_id) = tab_id {
                        let mux = Mux::get();
                        if let Some(tab) = mux.get_tab(tab_id) {
                            tab.set_title(&title);
                        }
                    }
                }
                Event::UnlinkedWindowClose { window } => {
                    let _ = self.remove_detached_window(*window);
                }
                _ => {}
            }
        }

        // send pending commands to tmux
        let cmd_queue = self.cmd_queue.as_ref().lock();
        if *self.state.lock() == State::Idle && !cmd_queue.is_empty() {
            TmuxDomainState::schedule_send_next_command(self.domain_id);
        }
    }

    /// send next command at the front of cmd_queue.
    /// must be called inside main thread
    fn send_next_command(&self) {
        if *self.state.lock() != State::Idle {
            return;
        }
        let mut cmd_queue = self.cmd_queue.as_ref().lock();
        while let Some(first) = cmd_queue.front() {
            let cmd = first.get_command(self.domain_id);
            if cmd.is_empty() {
                cmd_queue.pop_front();
                continue;
            }
            log::debug!("sending cmd {:?}", cmd);
            let mux = Mux::get();
            if let Some(pane) = mux.get_pane(self.pane_id) {
                let mut writer = pane.writer();
                let _ = write!(writer, "{}", cmd);
            }
            *self.state.lock() = State::WaitingForResponse;
            break;
        }
    }

    /// schedule a `send_next_command` into main thread
    pub fn schedule_send_next_command(domain_id: usize) {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            if let Some(domain) = mux.get_domain(domain_id) {
                if let Some(tmux_domain) = domain.downcast_ref::<TmuxDomain>() {
                    tmux_domain.send_next_command();
                }
            }
        })
        .detach();
    }

    /// create a standalone window for tmux tabs
    pub fn create_gui_window(&self) {
        if self.gui_window.lock().is_none() {
            let mux = Mux::get();
            let window_builder =
                if let Some((_domain, window_id, _tab)) = mux.resolve_pane_id(self.pane_id) {
                    MuxWindowBuilder {
                        window_id,
                        activity: Some(Activity::new()),
                        notified: false,
                    }
                } else {
                    mux.new_empty_window(
                        None, /* TODO: pass session here */
                        None, /* position */
                    )
                };

            log::info!("Tmux create window id {}", window_builder.window_id);
            {
                let mut window_id = self.gui_window.lock();
                *window_id = Some(window_builder); // keep the builder so it won't be purged
            }
        };
    }

    /// create a tmux window
    pub fn create_tmux_window(&self) {
        let mut cmd_queue = self.cmd_queue.as_ref().lock();
        cmd_queue.push_back(Box::new(NewWindow));
        TmuxDomainState::schedule_send_next_command(self.domain_id);
    }

    /// split the tmux pane
    pub fn split_tmux_pane(
        &self,
        _tab: TabId,
        pane_id: PaneId,
        split_request: SplitRequest,
    ) -> anyhow::Result<()> {
        let tmux_pane_id = self
            .remote_panes
            .lock()
            .iter()
            .find(|(_, ref_pane)| ref_pane.lock().local_pane_id == pane_id)
            .map(|p| p.1.lock().pane_id);

        if let Some(id) = tmux_pane_id {
            let mut cmd_queue = self.cmd_queue.as_ref().lock();
            cmd_queue.push_back(Box::new(SplitPane {
                pane_id: id,
                direction: split_request.direction,
            }));
            TmuxDomainState::schedule_send_next_command(self.domain_id);
            return Ok(());
        } else {
            anyhow::bail!("Could not find the tmux pane peer for local pane: {pane_id}");
        }
    }
}

impl TmuxDomain {
    pub fn new(pane_id: PaneId) -> Self {
        let domain_id = alloc_domain_id();
        let cmd_queue = VecDeque::new();
        let inner = Arc::new(TmuxDomainState {
            domain_id,
            pane_id,
            // parser,
            state: Mutex::new(State::WaitForInitialGuard),
            cmd_queue: Arc::new(Mutex::new(cmd_queue)),
            gui_window: Mutex::new(None),
            gui_tabs: Mutex::new(HashMap::default()),
            remote_panes: Mutex::new(HashMap::default()),
            tmux_session: Mutex::new(None),
            support_commands: Mutex::new(HashMap::default()),
            attach_state: Mutex::new(AttachState::Init),
            pending_splits: Mutex::new(VecDeque::default()),
            backlog: Mutex::new(HashMap::default()),
            handshake_buffers: Mutex::new(HashMap::default()),
            window_order: Mutex::new(Vec::new()),
        });

        Self { inner }
    }

    fn send_next_command(&self) {
        self.inner.send_next_command();
    }
}

#[async_trait(?Send)]
impl Domain for TmuxDomain {
    async fn spawn(
        &self,
        _size: TerminalSize,
        _command: Option<CommandBuilder>,
        _command_dir: Option<String>,
        _window: WindowId,
    ) -> anyhow::Result<Arc<Tab>> {
        self.inner.create_tmux_window();
        // This is intention, we would not return a Tab, since we don't have now!
        // We use create_tmux_window to create back end tmux window, then the
        // Tmux WindowAdd event will triage us to do the rest things.
        anyhow::bail!("Intention: we use tmux command to do so");
    }

    async fn split_pane(
        &self,
        _source: SplitSource,
        tab: TabId,
        pane_id: PaneId,
        split_request: SplitRequest,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        let mut promise = promise::Promise::new();
        if let Some(future) = promise.get_future() {
            {
                let mut pending_splits = self.inner.pending_splits.lock();
                let _ = self.inner.split_tmux_pane(tab, pane_id, split_request)?;
                pending_splits.push_back(promise);
            }

            if let Ok(id) = future.await {
                let pane = self.inner.split_pane(tab, pane_id, id, split_request);
                return pane;
            }
        }

        anyhow::bail!("Split_pane failed");
    }

    async fn spawn_pane(
        &self,
        _size: TerminalSize,
        _command: Option<CommandBuilder>,
        _command_dir: Option<String>,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        anyhow::bail!("Spawn_pane not yet implemented for TmuxDomain");
    }

    fn domain_id(&self) -> DomainId {
        self.inner.domain_id
    }

    fn domain_name(&self) -> &str {
        "tmux"
    }

    async fn attach(&self, _window_id: Option<crate::WindowId>) -> anyhow::Result<()> {
        Ok(())
    }

    fn detachable(&self) -> bool {
        false
    }

    fn detach(&self) -> anyhow::Result<()> {
        anyhow::bail!("detach not implemented for TmuxDomain");
    }

    fn state(&self) -> DomainState {
        DomainState::Attached
    }
}
