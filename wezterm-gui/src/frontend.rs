use crate::scripting::guiwin::GuiWin;
use crate::spawn::SpawnWhere;
use crate::termwindow::TermWindowNotif;
use crate::TermWindow;
use ::window::*;
use anyhow::{Context, Error};
use config::keyassignment::{KeyAssignment, SpawnCommand};
use config::{ConfigHandle, ConfigSubscription, NotificationHandling, ToastNotificationDuration};
use luahelper::from_lua_value_dynamic;
use mux::client::ClientId;
use mux::pane::PaneId;
use mux::window::WindowId as MuxWindowId;
use mux::{Mux, MuxNotification};
use mux_lua::MuxPane;
use promise::{Future, Promise};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use wezterm_dynamic::FromDynamic;
use wezterm_term::{Alert, ClipboardSelection};
use wezterm_toast_notification::*;

pub struct GuiFrontEnd {
    connection: Rc<Connection>,
    switching_workspaces: RefCell<bool>,
    spawned_mux_window: RefCell<HashSet<MuxWindowId>>,
    known_windows: RefCell<BTreeMap<Window, MuxWindowId>>,
    client_id: Arc<ClientId>,
    config_subscription: RefCell<Option<ConfigSubscription>>,
}

impl Drop for GuiFrontEnd {
    fn drop(&mut self) {
        ::window::shutdown();
    }
}

impl GuiFrontEnd {
    pub fn try_new() -> anyhow::Result<Rc<GuiFrontEnd>> {
        let connection = Connection::init()?;
        connection.set_event_handler(Self::app_event_handler);

        let mux = Mux::get();
        let client_id = mux.active_identity().expect("to have set my own id");

        let front_end = Rc::new(GuiFrontEnd {
            connection,
            switching_workspaces: RefCell::new(false),
            spawned_mux_window: RefCell::new(HashSet::new()),
            known_windows: RefCell::new(BTreeMap::new()),
            client_id: client_id.clone(),
            config_subscription: RefCell::new(None),
        });

        mux.subscribe(move |n| {
            match n {
                MuxNotification::WorkspaceRenamed {
                    old_workspace,
                    new_workspace,
                } => {
                    let mux = Mux::get();
                    let active = mux.active_workspace();
                    if active == old_workspace || active == new_workspace {
                        let switcher = WorkspaceSwitcher::new(&new_workspace);
                        promise::spawn::spawn_into_main_thread(async move {
                            drop(switcher);
                        })
                        .detach();
                    }
                }
                MuxNotification::WindowWorkspaceChanged(_)
                | MuxNotification::ActiveWorkspaceChanged(_)
                | MuxNotification::WindowCreated(_)
                | MuxNotification::WindowRemoved(_) => {
                    promise::spawn::spawn_into_main_thread(async move {
                        let fe = crate::frontend::front_end();
                        if !fe.is_switching_workspace() {
                            fe.reconcile_workspace();
                        }
                    })
                    .detach();
                }
                MuxNotification::PaneFocused(pane_id) => {
                    promise::spawn::spawn_into_main_thread(async move {
                        let mux = Mux::get();
                        if let Err(err) = mux.focus_pane_and_containing_tab(pane_id) {
                            log::error!("Error reconciling PaneFocused notification: {err:#}");
                        }
                    })
                    .detach();
                }
                MuxNotification::TabTitleChanged { .. } => {}
                MuxNotification::WindowTitleChanged { .. } => {}
                MuxNotification::TabResized(_) => {}
                MuxNotification::TabAddedToWindow { .. } => {}
                MuxNotification::PaneRemoved(pane_id) => {
                    // Clicking on its notification can no longer do anything
                    dismiss_pane_toast_notification(pane_id);
                }
                MuxNotification::WindowInvalidated(_) => {}
                MuxNotification::PaneOutput(_) => {}
                MuxNotification::PaneAdded(_) => {}
                MuxNotification::Alert {
                    pane_id,
                    alert: Alert::ToastNotification { title, body, focus },
                } => {
                    let client_id = client_id.clone();
                    promise::spawn::spawn_into_main_thread(async move {
                        handle_pane_toast_notification(
                            client_id,
                            PaneToastNotification {
                                pane_id,
                                title,
                                body,
                                focus,
                            },
                        );
                    })
                    .detach();
                }
                MuxNotification::Alert {
                    pane_id: _,
                    alert: Alert::Bell | Alert::Progress(_),
                } => {
                    // Handled via TermWindowNotif; NOP it here.
                }
                MuxNotification::Alert {
                    pane_id: _,
                    alert:
                        Alert::OutputSinceFocusLost
                        | Alert::PaletteChanged
                        | Alert::CurrentWorkingDirectoryChanged
                        | Alert::WindowTitleChanged(_)
                        | Alert::TabTitleChanged(_)
                        | Alert::IconTitleChanged(_)
                        | Alert::SetUserVar { .. },
                } => {}
                MuxNotification::Empty => {
                    if config::configuration().quit_when_all_windows_are_closed {
                        promise::spawn::spawn_into_main_thread(async move {
                            if mux::activity::Activity::count() == 0 {
                                log::trace!("Mux is now empty, terminate gui");
                                Connection::get().unwrap().terminate_message_loop();
                            }
                        })
                        .detach();
                    }
                }
                MuxNotification::SaveToDownloads { name, data } => {
                    if !config::configuration().allow_download_protocols {
                        log::error!(
                            "Ignoring download request for {:?}, \
                                 as allow_download_protocols=false",
                            name
                        );
                    } else if let Err(err) = crate::download::save_to_downloads(name, &*data) {
                        log::error!("save_to_downloads: {:#}", err);
                    }
                }
                MuxNotification::AssignClipboard {
                    pane_id,
                    selection,
                    clipboard,
                } => {
                    promise::spawn::spawn_into_main_thread(async move {
                        let fe = crate::frontend::front_end();
                        log::trace!(
                            "set clipboard in pane {} {:?} {:?}",
                            pane_id,
                            selection,
                            clipboard
                        );
                        // Route the assignment to the window that actually
                        // contains the pane that emitted OSC 52.
                        let target_window = {
                            let windows = fe.known_windows.borrow();
                            Mux::get()
                                .resolve_pane_id(pane_id)
                                .and_then(|(_domain, mux_window_id, _tab_id)| {
                                    windows
                                        .iter()
                                        .find(|(_window, id)| **id == mux_window_id)
                                        .map(|(window, _mux_window_id)| window.clone())
                                })
                                // The pane may not be shown by any window, in this case we fallback
                                // to any known window, rather than dropping the request.
                                // (note: the clipboard must be owned by a specific window)
                                .or_else(|| windows.keys().next().cloned())
                        };
                        if let Some(window) = target_window {
                            window.set_clipboard(
                                match selection {
                                    ClipboardSelection::Clipboard => Clipboard::Clipboard,
                                    ClipboardSelection::PrimarySelection => {
                                        Clipboard::PrimarySelection
                                    }
                                },
                                clipboard.unwrap_or_else(String::new),
                            );
                        } else {
                            log::error!("Cannot assign clipboard as there are no windows");
                        };
                    })
                    .detach();
                }
            }
            true
        });
        // Re-evaluate the config so that folks that are using
        // `wezterm.gui.get_appearance()` can have that take effect
        // before any windows are created
        config::reload();

        // And build the initial menu bar.
        // TODO: arrange for this to happen on config reload.
        crate::commands::CommandDef::recreate_menubar(&config::configuration());

        Ok(front_end)
    }

    fn app_event_handler(event: ApplicationEvent) {
        log::trace!("Got app event {event:?}");
        match event {
            ApplicationEvent::OpenCommandScript(file_name) => {
                let quoted_file_name = match shlex::try_quote(&file_name) {
                    Ok(name) => name.to_owned().to_string(),
                    Err(_) => {
                        log::error!(
                            "OpenCommandScript: {file_name} has embedded NUL bytes and
                             cannot be launched via the shell"
                        );
                        return;
                    }
                };
                promise::spawn::spawn(async move {
                    use config::keyassignment::SpawnTabDomain;
                    use wezterm_term::TerminalSize;

                    // We send the script to execute to the shell on stdin, rather than ask the
                    // shell to execute it directly, so that we start the shell and read in the
                    // user's rc files before running the script.  Without this, wezterm on macOS
                    // is launched with a default and very anemic path, and that is frustrating for
                    // users.

                    let mux = Mux::get();
                    let window_id = None;
                    let pane_id = None;
                    let cmd = None;
                    let cwd = None;
                    let workspace = mux.active_workspace();

                    match mux
                        .spawn_tab_or_window(
                            window_id,
                            SpawnTabDomain::DomainName("local".to_string()),
                            cmd,
                            cwd,
                            TerminalSize::default(),
                            pane_id,
                            workspace,
                            None, // optional position
                        )
                        .await
                    {
                        Ok((_tab, pane, _window_id)) => {
                            log::trace!("Spawned {file_name} as pane_id {}", pane.pane_id());
                            let mut writer = pane.writer();
                            write!(writer, "{quoted_file_name} ; exit\n").ok();
                        }
                        Err(err) => {
                            log::error!("Failed to spawn {file_name}: {err:#?}");
                        }
                    };
                })
                .detach();
            }
            ApplicationEvent::PerformKeyAssignment(action) => {
                // We should only get here when there are no windows open
                // and the user picks an action from the menubar.
                // This is not currently possible, but could be in the
                // future.

                fn spawn_command(spawn: &SpawnCommand, spawn_where: SpawnWhere) {
                    let config = config::configuration();
                    let dpi = config.dpi.unwrap_or_else(|| ::window::default_dpi());
                    let size =
                        config.initial_size(dpi as u32, crate::cell_pixel_dims(&config, dpi).ok());
                    let term_config = Arc::new(config::TermConfig::with_config(config));

                    crate::spawn::spawn_command_impl(spawn, spawn_where, size, None, term_config)
                }

                match action {
                    KeyAssignment::QuitApplication => {
                        // If we get here, there are no windows that could have received
                        // the QuitApplication command, therefore it must be ok to quit
                        // immediately
                        Connection::get().unwrap().terminate_message_loop();
                    }
                    KeyAssignment::SpawnWindow => {
                        spawn_command(&SpawnCommand::default(), SpawnWhere::NewWindow);
                    }
                    KeyAssignment::SpawnTab(spawn_where) => {
                        spawn_command(
                            &SpawnCommand {
                                domain: spawn_where,
                                ..Default::default()
                            },
                            SpawnWhere::NewWindow,
                        );
                    }
                    KeyAssignment::SpawnCommandInNewTab(spawn) => {
                        spawn_command(&spawn, SpawnWhere::NewTab);
                    }
                    KeyAssignment::SpawnCommandInNewWindow(spawn) => {
                        spawn_command(&spawn, SpawnWhere::NewWindow);
                    }
                    _ => {
                        log::warn!("unhandled perform: {action:?}");
                    }
                }
            }
        }
    }

    pub fn run_forever(&self) -> anyhow::Result<()> {
        self.connection
            .run_message_loop()
            .context("running message loop")
    }

    pub fn gui_windows(&self) -> Vec<GuiWin> {
        let windows = self.known_windows.borrow();
        let mut windows: Vec<GuiWin> = windows
            .iter()
            .map(|(window, &mux_window_id)| GuiWin {
                mux_window_id,
                window: window.clone(),
            })
            .collect();
        windows.sort_by(|a, b| a.window.cmp(&b.window));
        windows
    }

    pub fn reconcile_workspace(&self) -> Future<()> {
        let mut promise = Promise::new();
        let mux = Mux::get();
        let workspace = mux.active_workspace_for_client(&self.client_id);

        if mux.is_workspace_empty(&workspace) {
            // We don't want to silently kill off things that might
            // be running in other workspaces, so let's pick one
            // and activate it
            if self.is_switching_workspace() {
                promise.ok(());
                return promise.get_future().unwrap();
            }
            for workspace in mux.iter_workspaces() {
                if !mux.is_workspace_empty(&workspace) {
                    mux.set_active_workspace_for_client(&self.client_id, &workspace);
                    log::debug!("using {} instead, as it is not empty", workspace);
                    break;
                }
            }
        }

        let workspace = mux.active_workspace_for_client(&self.client_id);
        log::debug!("workspace is {}, fixup windows", workspace);

        let mut mux_windows = mux.iter_windows_in_workspace(&workspace);

        // First, repurpose existing windows.
        // Note that both iter_windows_in_workspace and self.known_windows have a
        // deterministic iteration order, so switching back and forth should result
        // in a consistent mux <-> gui window mapping.
        let known_windows = std::mem::take(&mut *self.known_windows.borrow_mut());
        let mut windows = BTreeMap::new();
        let mut unused = BTreeMap::new();

        for (window, window_id) in known_windows.into_iter() {
            if let Some(idx) = mux_windows.iter().position(|&id| id == window_id) {
                // it already points to the desired mux window
                windows.insert(window, window_id);
                mux_windows.remove(idx);
            } else {
                unused.insert(window, window_id);
            }
        }

        let mut mux_windows = mux_windows.into_iter();

        for (window, old_id) in unused.into_iter() {
            if let Some(mux_window_id) = mux_windows.next() {
                window.notify(TermWindowNotif::SwitchToMuxWindow(mux_window_id));
                windows.insert(window, mux_window_id);
            } else {
                // We have more windows than are in the new workspace;
                // we no longer need this one!
                window.close();
                front_end().spawned_mux_window.borrow_mut().remove(&old_id);
            }
        }

        log::trace!("reconcile: windows -> {:?}", windows);
        *self.known_windows.borrow_mut() = windows;

        let future = promise.get_future().unwrap();

        // then spawn any new windows that are needed
        promise::spawn::spawn(async move {
            while let Some(mux_window_id) = mux_windows.next() {
                if front_end().has_mux_window(mux_window_id)
                    || front_end()
                        .spawned_mux_window
                        .borrow()
                        .contains(&mux_window_id)
                {
                    continue;
                }
                front_end()
                    .spawned_mux_window
                    .borrow_mut()
                    .insert(mux_window_id);
                log::trace!("Creating TermWindow for mux_window_id={}", mux_window_id);
                if let Err(err) = TermWindow::new_window(mux_window_id).await {
                    log::error!("Failed to create window: {:#}", err);
                    let mux = Mux::get();
                    mux.kill_window(mux_window_id);
                    front_end()
                        .spawned_mux_window
                        .borrow_mut()
                        .remove(&mux_window_id);
                }
            }
            *front_end().switching_workspaces.borrow_mut() = false;
            promise.ok(());
        })
        .detach();
        future
    }

    fn has_mux_window(&self, mux_window_id: MuxWindowId) -> bool {
        for &mux_id in self.known_windows.borrow().values() {
            if mux_id == mux_window_id {
                return true;
            }
        }
        false
    }

    /// The returned future completes when the gui windows have been
    /// arranged to match the mux windows of the workspace
    pub fn switch_workspace(&self, workspace: &str) -> Future<()> {
        let mux = Mux::get();
        mux.set_active_workspace_for_client(&self.client_id, workspace);
        *self.switching_workspaces.borrow_mut() = false;
        self.reconcile_workspace()
    }

    pub fn record_known_window(&self, window: Window, mux_window_id: MuxWindowId) {
        self.known_windows
            .borrow_mut()
            .insert(window, mux_window_id);
        if !self.is_switching_workspace() {
            self.reconcile_workspace();
        }
    }

    pub fn forget_known_window(&self, window: &Window) {
        self.known_windows.borrow_mut().remove(window);
        if !self.is_switching_workspace() {
            self.reconcile_workspace();
        }
    }

    pub fn is_switching_workspace(&self) -> bool {
        *self.switching_workspaces.borrow()
    }

    pub fn gui_window_for_mux_window(&self, mux_window_id: MuxWindowId) -> Option<GuiWin> {
        let windows = self.known_windows.borrow();
        for (window, v) in windows.iter() {
            if *v == mux_window_id {
                return Some(GuiWin {
                    mux_window_id,
                    window: window.clone(),
                });
            }
        }
        None
    }
}

/// Maps the configured duration to its equivalent in the toast crate,
/// which doesn't know about the config crate.
pub fn configured_toast_duration(config: &ConfigHandle) -> ToastDuration {
    match config.toast_notification_duration {
        ToastNotificationDuration::Short => ToastDuration::Short,
        ToastNotificationDuration::Long => ToastDuration::Long,
    }
}

/// A toast notification that was requested by a pane via OSC 9 or OSC 777
struct PaneToastNotification {
    pane_id: PaneId,
    title: Option<String>,
    body: String,
    /// Whether clicking on the notification should focus the pane
    focus: bool,
}

/// Decides whether a toast notification that was requested by a pane
/// should be shown, and shows it if so.
/// Must be called on the main thread.
fn handle_pane_toast_notification(client_id: Arc<ClientId>, notif: PaneToastNotification) {
    let Some(mux) = Mux::try_get() else {
        return;
    };
    let pane_id = notif.pane_id;
    let Some((_domain, window_id, tab_id)) = mux.resolve_pane_id(pane_id) else {
        return;
    };

    let handling = config::configuration().notification_handling;
    match handling {
        NotificationHandling::NeverShow => return,
        NotificationHandling::AlwaysShow => {
            show_pane_toast_notification(notif);
            return;
        }
        NotificationHandling::SuppressFromFocusedPane
        | NotificationHandling::SuppressFromFocusedTab
        | NotificationHandling::SuppressFromFocusedWindow => {}
    }

    // The focused pane that the mux tracks for us is the pane that most
    // recently had the focus; it isn't cleared when our window loses the
    // focus. We only want to suppress the notification if the user is
    // actually looking at its source, so we must also check that the gui
    // window that shows the pane has the OS level focus, and only the
    // TermWindow knows that.
    let gui_window = try_front_end().and_then(|fe| fe.gui_window_for_mux_window(window_id));
    let Some(gui_window) = gui_window else {
        // Not shown by any gui window (eg: it is in another workspace),
        // so it cannot be focused
        show_pane_toast_notification(notif);
        return;
    };

    gui_window
        .window
        .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
            let os_focused =
                term_window.focused.is_some() && term_window.mux_window_id == window_id;

            let focused_pane = Mux::try_get().and_then(|mux| mux.resolve_focused_pane(&client_id));

            let suppress = os_focused
                && match focused_pane {
                    Some((_fdomain, f_window, f_tab, f_pane)) => match handling {
                        NotificationHandling::SuppressFromFocusedPane => f_pane == pane_id,
                        NotificationHandling::SuppressFromFocusedTab => f_tab == tab_id,
                        NotificationHandling::SuppressFromFocusedWindow => f_window == window_id,
                        NotificationHandling::AlwaysShow | NotificationHandling::NeverShow => false,
                    },
                    None => false,
                };

            if !suppress {
                show_pane_toast_notification(notif);
            }
        })));
}

/// What the `format-toast-notification` event may return to change
/// how the notification looks. Fields that aren't set keep their
/// default values.
#[derive(Debug, FromDynamic)]
struct ToastNotificationFormat {
    title: Option<String>,
    message: Option<String>,
    attribution: Option<String>,
    silent: Option<bool>,
    duration: Option<ToastNotificationDuration>,
}

/// Shows the toast notification that was requested by a pane, after
/// giving the `format-toast-notification` event the opportunity to
/// change how it looks, or to veto it.
/// Must be called on the main thread.
fn show_pane_toast_notification(notif: PaneToastNotification) {
    promise::spawn::spawn(config::with_lua_config_on_main_thread(move |lua| {
        format_and_show_pane_toast_notification(lua, notif)
    }))
    .detach();
}

async fn format_and_show_pane_toast_notification(
    lua: Option<Rc<mlua::Lua>>,
    notif: PaneToastNotification,
) -> anyhow::Result<()> {
    let pane_id = notif.pane_id;

    let mut toast = {
        let (title, message) = match notif.title.clone() {
            Some(title) => (title, notif.body.clone()),
            None => (notif.body.clone(), String::new()),
        };
        ToastNotification {
            title,
            message,
            url: None,
            timeout: None,
            duration: configured_toast_duration(&config::configuration()),
            on_click: if notif.focus {
                Some(ClickHandler::new(move || {
                    // We're called in the context of some thread that belongs
                    // to the notification system
                    promise::spawn::spawn_into_main_thread(async move {
                        focus_pane_for_notification(pane_id);
                    })
                    .detach();
                }))
            } else {
                None
            },
            attribution: None,
            silent: config::configuration().toast_notification_silent,
            // A pane has at most one notification at a time; a new one
            // replaces the one that it showed earlier
            tag: Some(pane_toast_notification_tag(pane_id)),
        }
    };

    if let Some(lua) = lua {
        match call_format_toast_notification(&lua, notif).await {
            Ok(mlua::Value::Nil) | Ok(mlua::Value::Boolean(true)) => {}
            Ok(mlua::Value::Boolean(false)) => {
                // The event handler doesn't want it to be shown
                return Ok(());
            }
            Ok(value) => match from_lua_value_dynamic::<ToastNotificationFormat>(value) {
                Ok(format) => {
                    if let Some(title) = format.title {
                        toast.title = title;
                    }
                    if let Some(message) = format.message {
                        toast.message = message;
                    }
                    toast.attribution = format.attribution.filter(|s| !s.is_empty());
                    if let Some(silent) = format.silent {
                        toast.silent = silent;
                    }
                    if let Some(duration) = format.duration {
                        toast.duration = match duration {
                            ToastNotificationDuration::Short => ToastDuration::Short,
                            ToastNotificationDuration::Long => ToastDuration::Long,
                        };
                    }
                }
                Err(err) => {
                    log::warn!("format-toast-notification returned an invalid value: {err:#}");
                }
            },
            // A broken event handler shouldn't cause notifications to be
            // lost, so we fall through and show it with the default look
            Err(err) => log::warn!("format-toast-notification: {err:#}"),
        }
    }

    PANES_WITH_TOAST_NOTIFICATION.with(|panes| panes.borrow_mut().insert(pane_id));
    toast.show();
    Ok(())
}

thread_local! {
    /// The panes that have shown a toast notification that we haven't
    /// dismissed since
    static PANES_WITH_TOAST_NOTIFICATION: RefCell<HashSet<PaneId>> = RefCell::new(HashSet::new());
    /// The pane that the user was most recently known to be looking at
    static LAST_FOCUSED_PANE: Cell<Option<PaneId>> = Cell::new(None);
}

fn pane_toast_notification_tag(pane_id: PaneId) -> String {
    format!("pane-{pane_id}")
}

/// Removes the toast notification that was shown for pane_id, if any.
/// Must be called on the main thread.
fn dismiss_pane_toast_notification(pane_id: PaneId) {
    let had_notification =
        PANES_WITH_TOAST_NOTIFICATION.with(|panes| panes.borrow_mut().remove(&pane_id));
    if had_notification {
        wezterm_toast_notification::dismiss(&pane_toast_notification_tag(pane_id));
    }
}

/// Called with the active pane of a window while that window has the
/// OS level focus, and with None when a window loses the focus.
/// When the user moves to a pane, the toast notification that it showed
/// earlier has served its purpose, so we remove it. We only do that at
/// the moment that the user moves to the pane, so that a notification
/// that is shown while its pane has the focus isn't removed immediately.
/// Must be called on the main thread.
pub fn note_focused_pane(pane_id: Option<PaneId>) {
    let changed = LAST_FOCUSED_PANE.with(|last| last.replace(pane_id) != pane_id);
    if changed {
        if let Some(pane_id) = pane_id {
            dismiss_pane_toast_notification(pane_id);
        }
    }
}

async fn call_format_toast_notification<'lua>(
    lua: &'lua mlua::Lua,
    notif: PaneToastNotification,
) -> mlua::Result<mlua::Value<'lua>> {
    // The gui window is nil if the pane isn't shown by any of them;
    // for example, when it is in a different workspace
    let gui_window = Mux::try_get()
        .and_then(|mux| mux.resolve_pane_id(notif.pane_id))
        .and_then(|(_domain, window_id, _tab_id)| {
            try_front_end().and_then(|fe| fe.gui_window_for_mux_window(window_id))
        });

    let notification = lua.create_table()?;
    notification.set("title", notif.title)?;
    notification.set("body", notif.body)?;

    config::lua::emit_async_callback(
        lua,
        (
            "format-toast-notification".to_string(),
            (gui_window, MuxPane(notif.pane_id), notification),
        ),
    )
    .await
}

/// Called when the toast notification that was generated by pane_id
/// is clicked: activates that pane and its tab, and brings the window
/// that contains it to the front.
/// If the pane has since been closed then we do nothing.
fn focus_pane_for_notification(pane_id: PaneId) {
    let Some(mux) = Mux::try_get() else {
        return;
    };
    let Some((_domain, window_id, _tab_id)) = mux.resolve_pane_id(pane_id) else {
        log::debug!("notification clicked, but pane {pane_id} no longer exists");
        return;
    };
    if let Err(err) = mux.focus_pane_and_containing_tab(pane_id) {
        log::debug!("notification clicked, but cannot focus pane {pane_id}: {err:#}");
        return;
    }
    let Some(fe) = try_front_end() else {
        return;
    };
    if let Some(gui_window) = fe.gui_window_for_mux_window(window_id) {
        raise_window_for_notification(gui_window);
        return;
    }

    // No gui window shows the pane, which means that it belongs to a
    // workspace that isn't the active one
    if !config::configuration().toast_notification_click_switches_workspace {
        return;
    }
    let workspace = mux
        .get_window(window_id)
        .map(|window| window.get_workspace().to_string());
    let Some(workspace) = workspace else {
        return;
    };
    let switched = fe.switch_workspace(&workspace);
    promise::spawn::spawn(async move {
        // Existing gui windows are re-used for the mux windows of the
        // workspace, and more are created if there aren't enough of them
        if let Err(err) = switched.await {
            log::debug!("notification clicked, but cannot switch to {workspace}: {err:#}");
            return;
        }
        if let Some(gui_window) =
            try_front_end().and_then(|fe| fe.gui_window_for_mux_window(window_id))
        {
            raise_window_for_notification(gui_window);
        }
    })
    .detach();
}

fn raise_window_for_notification(gui_window: GuiWin) {
    gui_window
        .window
        .notify(TermWindowNotif::Apply(Box::new(|term_window| {
            if let Some(window) = term_window.window.as_ref() {
                // Changing the active pane within the already active tab
                // doesn't invalidate the window by itself
                window.invalidate();
                // Focusing the window is intrusive on some systems (on
                // Windows it synthesizes a key press to be allowed to
                // take the foreground), so only do it when it is needed
                if term_window.focused.is_none() {
                    window.focus();
                }
            }
        })));
}

thread_local! {
    static FRONT_END: RefCell<Option<Rc<GuiFrontEnd>>> = RefCell::new(None);
}

pub fn try_front_end() -> Option<Rc<GuiFrontEnd>> {
    FRONT_END.with(|f| f.borrow().as_ref().map(Rc::clone))
}

pub fn front_end() -> Rc<GuiFrontEnd> {
    FRONT_END
        .with(|f| f.borrow().as_ref().map(Rc::clone))
        .expect("to be called on gui thread")
}

pub struct WorkspaceSwitcher {
    new_name: String,
}

impl WorkspaceSwitcher {
    pub fn new(new_name: &str) -> Self {
        *front_end().switching_workspaces.borrow_mut() = true;
        Self {
            new_name: new_name.to_string(),
        }
    }

    pub fn do_switch(self) {
        // Drop is invoked, which will complete the switch
    }
}

impl Drop for WorkspaceSwitcher {
    fn drop(&mut self) {
        front_end().switch_workspace(&self.new_name);
    }
}

pub fn shutdown() {
    FRONT_END.with(|f| drop(f.borrow_mut().take()));
}

pub fn try_new() -> Result<Rc<GuiFrontEnd>, Error> {
    let front_end = GuiFrontEnd::try_new()?;
    FRONT_END.with(|f| *f.borrow_mut() = Some(Rc::clone(&front_end)));

    let config_subscription = config::subscribe_to_config_reload({
        move || {
            promise::spawn::spawn_into_main_thread(async {
                crate::commands::CommandDef::recreate_menubar(&config::configuration());
            })
            .detach();
            true
        }
    });
    front_end
        .config_subscription
        .borrow_mut()
        .replace(config_subscription);

    Ok(front_end)
}
