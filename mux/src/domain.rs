//! A Domain represents an instance of a multiplexer.
//! For example, the gui frontend has its own domain,
//! and we can connect to a domain hosted by a mux server
//! that may be local, running "remotely" inside a WSL
//! container or actually remote, running on the other end
//! of an ssh session somewhere.

use crate::localpane::LocalPane;
use crate::pane::{alloc_pane_id, Pane, PaneId};
use crate::tab::{SplitRequest, Tab, TabId};
use crate::window::WindowId;
use crate::Mux;
use anyhow::{bail, Context, Error};
use async_trait::async_trait;
use config::keyassignment::{SpawnCommand, SpawnTabDomain};
use config::{configuration, ExecDomain, SerialDomain, ValueOrFunc, WslDomain};
use downcast_rs::{impl_downcast, Downcast};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, ExitStatus, MasterPty, PtySize, PtySystem};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wezterm_term::TerminalSize;

static DOMAIN_ID: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
pub type DomainId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainState {
    Detached,
    Attached,
}

pub fn alloc_domain_id() -> DomainId {
    DOMAIN_ID.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone, PartialEq)]
pub enum SplitSource {
    Spawn {
        command: Option<CommandBuilder>,
        command_dir: Option<String>,
    },
    MovePane(PaneId),
}

#[async_trait(?Send)]
pub trait Domain: Downcast + Send + Sync {
    /// Spawn a new command within this domain
    async fn spawn(
        &self,
        size: TerminalSize,
        command: Option<CommandBuilder>,
        command_dir: Option<String>,
        window: WindowId,
    ) -> anyhow::Result<Arc<Tab>> {
        let pane = self
            .spawn_pane(size, command, command_dir)
            .await
            .context("spawn")?;

        let tab = Arc::new(Tab::new(&size));
        tab.assign_pane(&pane);

        let mux = Mux::get();
        mux.add_tab_and_active_pane(&tab)?;
        mux.add_tab_to_window(&tab, window)?;

        Ok(tab)
    }

    async fn split_pane(
        &self,
        source: SplitSource,
        tab: TabId,
        pane_id: PaneId,
        split_request: SplitRequest,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        let mux = Mux::get();
        let tab = match mux.get_tab(tab) {
            Some(t) => t,
            None => anyhow::bail!("Invalid tab id {}", tab),
        };

        let pane_index = match tab
            .iter_panes_ignoring_zoom()
            .iter()
            .find(|p| p.pane.pane_id() == pane_id)
        {
            Some(p) => p.index,
            None => anyhow::bail!("invalid pane id {}", pane_id),
        };

        let split_size = match tab.compute_split_size(pane_index, split_request) {
            Some(s) => s,
            None => anyhow::bail!("invalid pane index {}", pane_index),
        };

        let pane = match source {
            SplitSource::Spawn {
                command,
                command_dir,
            } => {
                self.spawn_pane(split_size.second, command, command_dir)
                    .await?
            }
            SplitSource::MovePane(src_pane_id) => {
                let (_domain, _window, src_tab) = mux
                    .resolve_pane_id(src_pane_id)
                    .ok_or_else(|| anyhow::anyhow!("pane {} not found", src_pane_id))?;
                let src_tab = match mux.get_tab(src_tab) {
                    Some(t) => t,
                    None => anyhow::bail!("Invalid tab id {}", src_tab),
                };

                let pane = src_tab.remove_pane(src_pane_id).ok_or_else(|| {
                    anyhow::anyhow!("pane {} not found in its containing tab!?", src_pane_id)
                })?;

                if src_tab.is_dead() {
                    mux.remove_tab(src_tab.tab_id());
                }

                pane
            }
        };

        // pane_index may have changed if src_pane was also in the same tab
        let final_pane_index = match tab
            .iter_panes_ignoring_zoom()
            .iter()
            .find(|p| p.pane.pane_id() == pane_id)
        {
            Some(p) => p.index,
            None => anyhow::bail!("invalid pane id {}", pane_id),
        };

        tab.split_and_insert(final_pane_index, split_request, Arc::clone(&pane))?;
        Ok(pane)
    }

    async fn spawn_pane(
        &self,
        size: TerminalSize,
        command: Option<CommandBuilder>,
        command_dir: Option<String>,
    ) -> anyhow::Result<Arc<dyn Pane>>;

    /// The mux will call this method on the domain of the pane that
    /// is being moved to give the domain a chance to handle the movement.
    /// If this method returns Ok(None), then the mux will handle the
    /// movement itself by mutating its local Tabs and Windows.
    async fn move_pane_to_new_tab(
        &self,
        _pane_id: PaneId,
        _window_id: Option<WindowId>,
        _workspace_for_new_window: Option<String>,
    ) -> anyhow::Result<Option<(Arc<Tab>, WindowId)>> {
        Ok(None)
    }

    /// Returns false if the `spawn` method will never succeed.
    /// There are some internal placeholder domains that are
    /// pre-created with local UI that we do not want to allow
    /// to show in the launcher/menu as launchable items.
    fn spawnable(&self) -> bool {
        true
    }

    /// Returns true if the `detach` method can be used
    /// to detach the domain, preserving the associated
    /// panes, or false if the `detach` method will never
    /// succeed
    fn detachable(&self) -> bool;

    /// Returns the domain id, which is useful for obtaining
    /// a handle on the domain later.
    fn domain_id(&self) -> DomainId;

    /// Returns the name of the domain.
    /// Should be a short identifier.
    fn domain_name(&self) -> &str;

    /// Returns a label describing the domain.
    async fn domain_label(&self) -> String {
        self.domain_name().to_string()
    }

    /// Re-attach to any tabs that might be pre-existing in this domain
    async fn attach(&self, window_id: Option<WindowId>) -> anyhow::Result<()>;

    /// Detach all tabs
    fn detach(&self) -> anyhow::Result<()>;

    /// Indicates the state of the domain
    fn state(&self) -> DomainState;
}
impl_downcast!(Domain);

pub struct LocalDomain {
    pty_system: Mutex<Box<dyn PtySystem + Send>>,
    id: DomainId,
    name: String,
    /// Whether this domain was constructed via `new_wsl` -- ie. it's a WSL
    /// domain, whether from explicit `wsl_domains` config or from
    /// background discovery. Used only to short-circuit `resolve_wsl_domain`
    /// for non-WSL domains without touching global config at all: that
    /// method used to call `config.wsl_domains()` unconditionally for
    /// *every* LocalDomain (including plain non-WSL ones like "local"),
    /// which falls back to `WslDomain::default_domains()` and could shell
    /// out to `wsl.exe` when `wsl_domains` isn't explicitly configured,
    /// meaning every pane spawn paid that cost regardless of what kind of
    /// domain it was spawning into.
    ///
    /// Deliberately not a cached snapshot of the `WslDomain` itself:
    /// `resolve_wsl_domain` always re-reads the live, current
    /// `config.wsl_domains_cached()` by name instead, matching this
    /// method's pre-existing (always-live) behavior. Caching a snapshot
    /// here was tried and reverted -- it made a domain registered under an
    /// explicit `wsl_domains` entry keep using that entry's settings
    /// forever even after the entry (or `wsl_domains` entirely) was
    /// removed from the config on reload, and made a domain registered by
    /// auto-discovery *not* revert to a plain non-WSL shell when
    /// `wsl_domains = {}` -- the documented way to disable auto-added WSL
    /// domains -- was set on reload. `wsl_domains_cached()` is the
    /// non-blocking variant (see its doc comment): it never shells out to
    /// `wsl.exe` itself, so the live lookup costs nothing beyond a mutex
    /// lock once we know this domain is WSL-flavored in the first place.
    is_wsl_domain: bool,
    /// Construction-time snapshot of critical WSL domain fields, used as a
    /// fallback ONLY while `wsl_domains_cached()` returns `None`.
    ///
    /// A domain registered *by* auto-discovery can't get here: the ordering in
    /// `config::WslDomain::try_default_domains()` publishes the cache before
    /// the domains are registered, so by the time one exists to spawn into, the
    /// lookup it does has an answer. What does reach it is a domain that
    /// outlived the config that named it -- registered from an explicit
    /// `wsl_domains` list, then that list removed on reload, which leaves the
    /// domain in the mux (domains are never unregistered) and starts a
    /// discovery whose cache isn't published yet. Every spawn into it until
    /// that discovery lands takes this path.
    wsl_fallback: Option<WslDomainFallback>,
}

/// Snapshot of critical WSL domain fields stored at construction time.
/// Used ONLY as a fallback for the "cache isn't populated yet" case;
/// never substitutes for a live cached lookup. See `wsl_fallback` for when
/// that case actually arises.
#[derive(Clone, Debug)]
struct WslDomainFallback {
    name: String,
    distribution: Option<String>,
    username: Option<String>,
    default_cwd: Option<PathBuf>,
    default_prog: Option<Vec<String>>,
}

impl LocalDomain {
    pub fn new(name: &str) -> Result<Self, Error> {
        Ok(Self::with_pty_system(name, native_pty_system()))
    }

    fn resolve_exec_domain(&self) -> Option<ExecDomain> {
        config::configuration()
            .exec_domains
            .iter()
            .find(|ed| ed.name == self.name)
            .cloned()
    }

    fn resolve_wsl_domain(&self) -> Option<WslDomain> {
        // Short-circuits before touching global config at all for a
        // non-WSL domain, which is the point: this used to unconditionally
        // call `config.wsl_domains()` for every domain type, including
        // plain "local" ones.
        if !self.is_wsl_domain {
            return None;
        }
        // Use the non-blocking cached accessor: pane-spawn must never
        // shell out to `wsl.exe`. Two distinct "not found" cases have to be
        // told apart here:
        //
        // - the list is known (explicit config, or auto-discovery already
        //   completed) but doesn't mention this domain's name -- eg. its
        //   `wsl_domains` entry was removed on reload, or `wsl_domains = {}`
        //   was set to disable auto-added WSL domains. That's a real "this
        //   is no longer a WSL domain" and must resolve to `None`, exactly
        //   as before (`wsl_domain_with_no_matching_entry_resolves_to_none`
        //   below locks this in).
        // - the list isn't known *yet*: most often because this domain came
        //   from an explicit `wsl_domains` list that a reload has since
        //   removed, leaving it registered while the discovery that replaced
        //   it is still running (see `wsl_fallback`). We still know from
        //   `is_wsl_domain` that this
        //   domain is WSL-flavored, so we must not let `fixup_command` fall through
        //   to its "not WSL at all" branch, which runs the command directly on the
        //   Windows host instead of inside WSL -- silently wrong, not just slow.
        //   Fall back to a `WslDomain` built from the construction-time snapshot
        //   stored in `wsl_fallback`, which contains the actual distribution name
        //   (not just the mux domain name) and any other per-distro overrides
        //   configured at registration time.
        self.resolve_wsl_domain_from_cache(config::configuration().wsl_domains_cached().as_deref())
    }

    /// The config-free core of `resolve_wsl_domain`: takes the cached list
    /// (or `None` as a defensive guard against future reordering) as a parameter
    /// rather than reading process-global config, so both branches can be
    /// unit tested directly. Without this seam a test of the `None` branch
    /// would have to rely on the global cache happening to be unpopulated
    /// in the test binary -- true today, but silently broken by the first
    /// test that ever triggers a real discovery.
    fn resolve_wsl_domain_from_cache(&self, cached: Option<&[WslDomain]>) -> Option<WslDomain> {
        match cached {
            Some(wsl_domains) => Self::resolve_wsl_domain_impl(&self.name, wsl_domains),
            None => self.wsl_fallback.as_ref().map(|f| WslDomain {
                name: f.name.clone(),
                distribution: f.distribution.clone(),
                username: f.username.clone(),
                default_cwd: f.default_cwd.clone(),
                default_prog: f.default_prog.clone(),
            }),
        }
    }

    /// The name-lookup half of `resolve_wsl_domain_from_cache`, split out
    /// so it can be tested without constructing a `LocalDomain`.
    fn resolve_wsl_domain_impl(name: &str, wsl_domains: &[WslDomain]) -> Option<WslDomain> {
        wsl_domains.iter().find(|d| d.name == name).cloned()
    }

    pub fn with_pty_system(name: &str, pty_system: Box<dyn PtySystem + Send>) -> Self {
        let id = alloc_domain_id();
        Self {
            pty_system: Mutex::new(pty_system),
            id,
            name: name.to_string(),
            is_wsl_domain: false,
            wsl_fallback: None,
        }
    }

    pub fn new_wsl(wsl: WslDomain) -> Result<Self, Error> {
        let mut domain = Self::new(&wsl.name)?;
        domain.is_wsl_domain = true;
        domain.wsl_fallback = Some(WslDomainFallback {
            name: wsl.name.clone(),
            distribution: wsl.distribution.clone(),
            username: wsl.username.clone(),
            default_cwd: wsl.default_cwd.clone(),
            default_prog: wsl.default_prog.clone(),
        });
        Ok(domain)
    }

    pub fn new_exec_domain(exec_domain: ExecDomain) -> anyhow::Result<Self> {
        Self::new(&exec_domain.name)
    }

    pub fn new_serial_domain(serial_domain: SerialDomain) -> anyhow::Result<Self> {
        let port = serial_domain.port.as_ref().unwrap_or(&serial_domain.name);
        let mut serial = portable_pty::serial::SerialTty::new(&port);
        if let Some(baud) = serial_domain.baud {
            serial.set_baud_rate(baud as u32);
        }
        let pty_system = Box::new(serial);
        Ok(Self::with_pty_system(&serial_domain.name, pty_system))
    }

    #[cfg(unix)]
    fn is_conpty(&self) -> bool {
        false
    }

    #[cfg(windows)]
    fn is_conpty(&self) -> bool {
        let pty_system = self.pty_system.lock();
        let pty_system: &dyn PtySystem = &**pty_system;
        pty_system
            .downcast_ref::<portable_pty::win::conpty::ConPtySystem>()
            .is_some()
    }

    async fn fixup_command(&self, cmd: &mut CommandBuilder) -> anyhow::Result<()> {
        if let Some(wsl) = self.resolve_wsl_domain() {
            let mut args: Vec<OsString> = cmd.get_argv().clone();

            if args.is_empty() {
                if let Some(def_prog) = &wsl.default_prog {
                    for arg in def_prog {
                        args.push(arg.into());
                    }
                }
            }

            let mut argv: Vec<OsString> = vec![
                "wsl.exe".into(),
                "--distribution".into(),
                wsl.distribution
                    .as_deref()
                    .unwrap_or(wsl.name.as_str())
                    .into(),
            ];

            if let Some(cwd) = cmd.get_cwd() {
                argv.push("--cd".into());
                argv.push(cwd.into());
            }

            if let Some(user) = &wsl.username {
                argv.push("--user".into());
                argv.push(user.into());
            }

            if !args.is_empty() {
                argv.push("--exec".into());
                for arg in args {
                    argv.push(arg);
                }
            }

            // TODO: process env list and update WLSENV so that they
            // get passed through

            cmd.clear_cwd();
            *cmd.get_argv_mut() = argv;
        } else if let Some(ed) = self.resolve_exec_domain() {
            let mut args = vec![];
            let mut set_environment_variables = HashMap::new();
            for arg in cmd.get_argv() {
                args.push(
                    arg.to_str()
                        .ok_or_else(|| anyhow::anyhow!("command argument is not utf8"))?
                        .to_string(),
                );
            }
            for (k, v) in cmd.iter_full_env_as_str() {
                set_environment_variables.insert(k.to_string(), v.to_string());
            }
            let cwd = match cmd.get_cwd() {
                Some(cwd) => Some(PathBuf::from(cwd)),
                None => None,
            };
            let spawn_command = SpawnCommand {
                label: None,
                domain: SpawnTabDomain::DomainName(ed.name.clone()),
                args: if args.is_empty() { None } else { Some(args) },
                set_environment_variables,
                cwd,
                position: None,
            };

            let spawn_command = config::with_lua_config_on_main_thread(|lua| async {
                let lua = lua.ok_or_else(|| anyhow::anyhow!("missing lua context"))?;
                let value = config::lua::emit_async_callback(
                    &*lua,
                    (ed.fixup_command.clone(), (spawn_command.clone())),
                )
                .await?;
                let cmd: SpawnCommand =
                    luahelper::from_lua_value_dynamic(value).with_context(|| {
                        format!(
                            "interpreting SpawnCommand result from ExecDomain {}",
                            ed.name
                        )
                    })?;
                Ok(cmd)
            })
            .await
            .with_context(|| format!("calling ExecDomain {} function", ed.name))?;

            // Reinterpret the SpawnCommand into the builder

            cmd.get_argv_mut().clear();
            if let Some(args) = &spawn_command.args {
                for arg in args {
                    cmd.get_argv_mut().push(arg.into());
                }
            }
            cmd.env_clear();
            for (k, v) in &spawn_command.set_environment_variables {
                cmd.env(k, v);
            }
            cmd.clear_cwd();
            if let Some(cwd) = &spawn_command.cwd {
                cmd.cwd(cwd);
            }
        } else if Path::new("/.flatpak-info").exists() {
            // We're running inside a flatpak sandbox.
            // Run the command outside the sandbox via flatpak-spawn
            let mut args = vec![
                "flatpak-spawn".to_string(),
                "--host".to_string(),
                "--watch-bus".to_string(),
            ];
            if let Some(cwd) = cmd.get_cwd() {
                args.push(format!("--directory={}", Path::new(cwd).display()));
            }

            let is_default_prog = cmd.is_default_prog();

            // Note: WEZTERM_UNIX_SOCKET, WEZTERM_CONFIG_(FILE|DIR) and other env
            // vars are not included in this.
            // We can't include them: their paths are only meaningful in the sandbox
            // and cannot be reasonably accessed from outside it in the shell.
            for (k, v) in cmd.iter_extra_env_as_str() {
                args.push(format!("--env={k}={v}"));
            }

            for arg in cmd.get_argv() {
                args.push(
                    arg.to_str()
                        .ok_or_else(|| anyhow::anyhow!("command argument is not utf8"))?
                        .to_string(),
                );
            }

            if is_default_prog {
                // We can't read $SHELL from inside the sandbox, so ask the host.
                let output = std::process::Command::new("flatpak-spawn")
                    .args(["--host", "sh", "-c", "echo $SHELL"])
                    .output()?;
                let shell = String::from_utf8_lossy(&output.stdout);

                args.push(shell.trim().to_string());
                // Assume we can pass `-l` for a login shell
                args.push("-l".to_string());
            }

            // Avoid setting up the controlling tty as that is not compatible
            // with flatpak:
            // <https://github.com/flatpak/flatpak/issues/3697>
            // <https://github.com/flatpak/flatpak/issues/3285>
            cmd.set_controlling_tty(false);

            // Re-apply to the builder
            cmd.get_argv_mut().clear();
            for arg in args {
                cmd.get_argv_mut().push(arg.into());
            }
            cmd.clear_cwd();
            log::trace!("made: {cmd:#?}");
        } else if let Some(dir) = cmd.get_cwd() {
            // I'm not normally a fan of existence checking, but not checking here
            // can be painful; in the case where a tab is local but has connected
            // to a remote system and that remote has used OSC 7 to set a path
            // that doesn't exist on the local system, process spawning can fail.
            // Another situation is `sudo -i` has the pane with set to a cwd
            // that is not accessible to the user.
            if let Err(err) = Path::new(&dir).read_dir() {
                log::warn!(
                    "Directory {:?} is not readable and will not be \
                     used for the command we are spawning: {:#}",
                    dir,
                    err
                );
                cmd.clear_cwd();
            }
        }
        Ok(())
    }

    async fn build_command(
        &self,
        command: Option<CommandBuilder>,
        command_dir: Option<String>,
        pane_id: PaneId,
    ) -> anyhow::Result<CommandBuilder> {
        let config = configuration();

        let wsl = self.resolve_wsl_domain();
        let default_prog = wsl
            .as_ref()
            .map(|wsl| wsl.default_prog.as_ref())
            .unwrap_or(config.default_prog.as_ref());

        let mut cmd = match command {
            Some(mut cmd) => {
                config.apply_cmd_defaults(&mut cmd, default_prog, config.default_cwd.as_ref());
                cmd
            }
            None => config.build_prog(
                None,
                default_prog,
                wsl.as_ref()
                    .map(|wsl| wsl.default_cwd.as_ref())
                    .unwrap_or(config.default_cwd.as_ref()),
            )?,
        };
        if let Some(dir) = command_dir {
            cmd.cwd(dir);
        }
        if let Ok(sock) = std::env::var("WEZTERM_UNIX_SOCKET") {
            cmd.env("WEZTERM_UNIX_SOCKET", sock);
        }
        cmd.env("WEZTERM_PANE", pane_id.to_string());
        if let Some(agent) = Mux::get().agent.as_ref() {
            cmd.env("SSH_AUTH_SOCK", agent.path());
        }
        self.fixup_command(&mut cmd).await?;
        Ok(cmd)
    }
}

/// Allows sharing the writer between the Pane and the Terminal.
/// This could potentially be eliminated in the future if we can
/// teach the Pane impl to reference the writer in the Termninal,
/// but the Pane trait returns a RefMut and that makes it a bit
/// awkward at the moment.
#[derive(Clone)]
pub(crate) struct WriterWrapper {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl WriterWrapper {
    pub fn new(writer: Box<dyn Write + Send>) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
        }
    }
}

impl std::io::Write for WriterWrapper {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.lock().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.lock().flush()
    }
}

/// Wraps the underlying pty; we use this as a marker for when
/// the spawn attempt failed in order to hold the pane open
pub(crate) struct FailedSpawnPty {
    inner: Mutex<Box<dyn MasterPty>>,
}

impl portable_pty::MasterPty for FailedSpawnPty {
    fn resize(&self, new_size: PtySize) -> anyhow::Result<()> {
        self.inner.lock().resize(new_size)
    }
    fn get_size(&self) -> anyhow::Result<PtySize> {
        self.inner.lock().get_size()
    }
    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send + 'static>> {
        self.inner.lock().try_clone_reader()
    }
    fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send + 'static>> {
        self.inner.lock().take_writer()
    }

    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        None
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

/// A fake child process for the case where the spawn attempt
/// failed. It reports as immediately terminated.
#[derive(Debug)]
pub(crate) struct FailedProcessSpawn {}

impl portable_pty::Child for FailedProcessSpawn {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        Ok(Some(ExitStatus::with_exit_code(1)))
    }

    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        Ok(ExitStatus::with_exit_code(1))
    }

    fn process_id(&self) -> Option<u32> {
        None
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

impl portable_pty::ChildKiller for FailedProcessSpawn {
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(FailedProcessSpawn {})
    }
}

#[async_trait(?Send)]
impl Domain for LocalDomain {
    async fn spawn_pane(
        &self,
        size: TerminalSize,
        command: Option<CommandBuilder>,
        command_dir: Option<String>,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        let pane_id = alloc_pane_id();
        let cmd = self
            .build_command(command, command_dir, pane_id)
            .await
            .context("build_command")?;
        let pair = self
            .pty_system
            .lock()
            .openpty(crate::terminal_size_to_pty_size(size)?)?;

        let command_line = cmd
            .as_unix_command_line()
            .unwrap_or_else(|err| format!("error rendering command line: {:?}", err));
        let command_description = format!(
            "\"{}\" in domain \"{}\"",
            if command_line.is_empty() {
                cmd.get_shell()
            } else {
                command_line
            },
            self.name
        );
        let child_result = pair.slave.spawn_command(cmd);
        let mut writer = WriterWrapper::new(pair.master.take_writer()?);

        let mut terminal = wezterm_term::Terminal::new(
            size,
            std::sync::Arc::new(config::TermConfig::new()),
            "WezTerm",
            config::wezterm_version(),
            Box::new(writer.clone()),
        );
        if self.is_conpty() {
            terminal.enable_conpty_quirks();
        }

        let pane: Arc<dyn Pane> = match child_result {
            Ok(child) => Arc::new(LocalPane::new(
                pane_id,
                terminal,
                child,
                pair.master,
                Box::new(writer),
                self.id,
                command_description,
            )),
            Err(err) => {
                // Show the error to the user in the new pane
                write!(writer, "{err:#}").ok();

                // and return a dummy pane that has exited
                Arc::new(LocalPane::new(
                    pane_id,
                    terminal,
                    Box::new(FailedProcessSpawn {}),
                    Box::new(FailedSpawnPty {
                        inner: Mutex::new(pair.master),
                    }),
                    Box::new(writer),
                    self.id,
                    command_description,
                ))
            }
        };

        let mux = Mux::get();
        mux.add_pane(&pane)?;

        Ok(pane)
    }

    fn domain_id(&self) -> DomainId {
        self.id
    }

    fn domain_name(&self) -> &str {
        &self.name
    }

    async fn domain_label(&self) -> String {
        if let Some(ed) = self.resolve_exec_domain() {
            match &ed.label {
                Some(ValueOrFunc::Value(wezterm_dynamic::Value::String(s))) => s.to_string(),
                Some(ValueOrFunc::Func(label_func)) => {
                    let label = config::with_lua_config_on_main_thread(|lua| async {
                        let lua = lua.ok_or_else(|| anyhow::anyhow!("missing lua context"))?;
                        let value = config::lua::emit_async_callback(
                            &*lua,
                            (label_func.clone(), (self.name.clone())),
                        )
                        .await?;
                        let label: String =
                            luahelper::from_lua_value_dynamic(value).with_context(|| {
                                format!(
                                    "interpreting SpawnCommand result from ExecDomain {}",
                                    ed.name
                                )
                            })?;
                        Ok(label)
                    })
                    .await;
                    match label {
                        Ok(label) => label,
                        Err(err) => {
                            log::error!(
                                "Error while calling label function for ExecDomain `{}`: {err:#}",
                                self.name
                            );
                            self.name.to_string()
                        }
                    }
                }
                _ => self.name.to_string(),
            }
        } else if let Some(wsl) = self.resolve_wsl_domain() {
            wsl.distribution.unwrap_or_else(|| self.name.to_string())
        } else {
            self.name.to_string()
        }
    }

    async fn attach(&self, _window_id: Option<WindowId>) -> anyhow::Result<()> {
        Ok(())
    }

    fn detachable(&self) -> bool {
        false
    }

    fn detach(&self) -> anyhow::Result<()> {
        bail!("detach not implemented for LocalDomain");
    }

    fn state(&self) -> DomainState {
        DomainState::Attached
    }
}

#[cfg(test)]
mod resolve_wsl_domain_tests {
    use super::*;

    fn wsl_domain(name: &str, default_prog: Option<Vec<&str>>) -> WslDomain {
        WslDomain {
            name: name.to_string(),
            distribution: None,
            username: None,
            default_cwd: None,
            default_prog: default_prog.map(|p| p.into_iter().map(String::from).collect()),
        }
    }

    /// A plain (non-WSL) domain must never consult global config to
    /// resolve WSL settings: this is what keeps a `local` pane spawn from
    /// ever shelling out to `wsl.exe`, regardless of what background
    /// discovery is doing concurrently.
    #[test]
    fn plain_domain_resolves_to_none_without_touching_config() {
        let domain = LocalDomain::new("local").expect("LocalDomain::new is infallible in tests");
        assert!(domain.resolve_wsl_domain().is_none());
    }

    /// A WSL domain resolves to whatever entry in the current, live
    /// `config.wsl_domains()` list matches its name -- always live, never
    /// a value cached at construction time. See the doc comment on
    /// `LocalDomain::is_wsl_domain` for why: caching used to make a
    /// domain keep stale settings (or stay WSL-flavored when it shouldn't
    /// be) across a config reload that changed or removed its
    /// configuration.
    #[test]
    fn wsl_domain_resolves_matching_entry_by_name() {
        let ubuntu = wsl_domain("WSL:Ubuntu", Some(vec!["bash"]));
        let debian = wsl_domain("WSL:Debian", Some(vec!["zsh"]));

        let resolved =
            LocalDomain::resolve_wsl_domain_impl("WSL:Ubuntu", &[ubuntu.clone(), debian]);
        assert_eq!(resolved.map(|d| d.default_prog), Some(ubuntu.default_prog));
    }

    /// Regression test: if the current effective
    /// `wsl_domains` list (explicit or auto-discovered) no longer
    /// mentions this domain's name -- eg. its `wsl_domains` entry was
    /// removed on reload, or `wsl_domains = {}` was set to disable
    /// auto-discovered WSL domains -- resolution must return `None`
    /// rather than silently keep serving a stale cached value. A cached
    /// snapshot was tried and reverted specifically because it broke
    /// this case.
    #[test]
    fn wsl_domain_with_no_matching_entry_resolves_to_none() {
        let debian = wsl_domain("WSL:Debian", Some(vec!["zsh"]));
        let resolved = LocalDomain::resolve_wsl_domain_impl("WSL:Ubuntu", &[debian]);
        assert!(resolved.is_none());
    }

    /// `resolve_wsl_domain_impl` (the pure name-lookup core) resolving
    /// against an empty-but-known list must return `None` -- this is the
    /// legitimate "discovery completed and found nothing" / "name genuinely
    /// absent" case, distinct from "discovery hasn't populated the cache
    /// yet" (see `resolve_wsl_domain_falls_back_to_minimal_domain_while_cache_is_unpopulated`
    /// below for that case, which must NOT resolve to `None`).
    #[test]
    fn resolve_wsl_domain_impl_with_empty_known_list_resolves_to_none() {
        let empty: &[WslDomain] = &[];
        let resolved = LocalDomain::resolve_wsl_domain_impl("WSL:Ubuntu", empty);
        assert!(
            resolved.is_none(),
            "an empty but *known* list (eg. discovery found zero distros) must resolve to None"
        );
    }

    /// When the cache is populated (simulating completed discovery),
    /// `resolve_wsl_domain` must return the matching entry from the cache.
    #[test]
    fn resolve_wsl_domain_uses_cached_value_when_available() {
        let ubuntu = wsl_domain("WSL:Ubuntu", Some(vec!["bash"]));
        let debian = wsl_domain("WSL:Debian", Some(vec!["zsh"]));

        let cached = vec![ubuntu.clone(), debian];
        let resolved = LocalDomain::resolve_wsl_domain_impl("WSL:Ubuntu", &cached);
        assert_eq!(resolved.map(|d| d.default_prog), Some(ubuntu.default_prog));
    }

    /// Regression test for the explicit -> unset config reload race: while
    /// auto-discovery hasn't populated the cache yet (`wsl_domains_cached()`
    /// returns `None`, not `Some(vec![])`), `resolve_wsl_domain` must still
    /// report this domain as WSL-flavored and must preserve the *actual*
    /// distribution name from construction time, not the mux domain name.
    /// `fixup_command` gates its entire `wsl.exe --distribution ...` wrapping
    /// on `resolve_wsl_domain` returning `Some`, so returning `None` here would
    /// make a real WSL domain's pane spawn run directly on the Windows host
    /// instead of inside WSL: silently wrong, not just slow. And if the
    /// distribution name is wrong, the spawn fails with "distribution not found"
    /// instead of working.
    #[test]
    fn resolve_wsl_domain_uses_fallback_with_correct_distribution() {
        // Create a domain where the mux name differs from the actual distribution,
        // simulating `{ name = "WSL:Ubuntu", distribution = "Ubuntu" }` from config.
        let wsl_config = WslDomain {
            name: "WSL:Ubuntu".to_string(),
            distribution: Some("Ubuntu".to_string()),
            username: Some("ubuntuuser".to_string()),
            default_cwd: Some("/home/ubuntuuser".into()),
            default_prog: Some(vec!["bash".to_string()]),
        };

        let domain = LocalDomain::new_wsl(wsl_config.clone())
            .expect("LocalDomain::new_wsl is infallible in tests");

        // Passes the "cache not populated yet" state in explicitly rather
        // than relying on the process-global config cache happening to be
        // empty in this test binary: that happens to be true today (nothing
        // here runs a real discovery), but it's an invariant owned by other
        // tests, and the first one to break it would turn this into a
        // confusing failure somewhere else.
        let resolved = domain.resolve_wsl_domain_from_cache(None);

        assert!(
            resolved.is_some(),
            "WSL domain must resolve to Some while cache is unpopulated"
        );
        let resolved = resolved.unwrap();

        // The critical assertion: distribution must be the actual distro name
        // from construction, NOT the mux name. This is what prevents the
        // `wsl.exe --distribution WSL:Ubuntu` bug (correct is `--distribution Ubuntu`).
        assert_eq!(
            resolved.distribution,
            Some("Ubuntu".to_string()),
            "fallback must preserve the actual distribution name from construction time"
        );

        // Also verify the other fields are preserved for consistency.
        assert_eq!(resolved.name, "WSL:Ubuntu");
        assert_eq!(resolved.username, Some("ubuntuuser".to_string()));
        assert_eq!(resolved.default_cwd, Some("/home/ubuntuuser".into()));
        assert_eq!(resolved.default_prog, Some(vec!["bash".to_string()]));
    }
}
