use config::keyassignment::SpawnTabDomain;
use config::lua::mlua::{self, Lua, UserData, UserDataMethods, Value as LuaValue};
use config::lua::{get_or_create_module, get_or_create_sub_module};
use luahelper::impl_lua_conversion_dynamic;
use mlua::UserDataRef;
use mux::domain::{DomainId, SplitSource};
use mux::pane::{Pane, PaneId};
use mux::tab::{SplitDirection, SplitRequest, SplitSize, Tab, TabId};
use mux::window::{Window, WindowId};
use mux::Mux;
use portable_pty::CommandBuilder;
use std::collections::HashMap;
use std::sync::Arc;
use wezterm_dynamic::{FromDynamic, ToDynamic};
use wezterm_term::TerminalSize;

mod domain;
mod pane;
mod tab;
mod window;

pub use domain::MuxDomain;
pub use pane::MuxPane;
pub use tab::MuxTab;
pub use window::MuxWindow;

fn get_mux() -> mlua::Result<Arc<Mux>> {
    Mux::try_get().ok_or_else(|| mlua::Error::external("cannot get Mux!?"))
}

pub fn register(lua: &Lua) -> anyhow::Result<()> {
    let mux_mod = get_or_create_sub_module(lua, "mux")?;

    mux_mod.set(
        "get_active_workspace",
        lua.create_function(|_, _: ()| {
            let mux = get_mux()?;
            Ok(mux.active_workspace())
        })?,
    )?;

    mux_mod.set(
        "get_workspace_names",
        lua.create_function(|_, _: ()| {
            let mux = get_mux()?;
            Ok(mux.iter_workspaces())
        })?,
    )?;

    mux_mod.set(
        "set_active_workspace",
        lua.create_function(|_, workspace: String| {
            let mux = get_mux()?;
            let workspaces = mux.iter_workspaces();
            if workspaces.contains(&workspace) {
                Ok(mux.set_active_workspace(&workspace))
            } else {
                Err(mlua::Error::external(format!(
                    "{:?} is not an existing workspace",
                    workspace
                )))
            }
        })?,
    )?;

    mux_mod.set(
        "rename_workspace",
        lua.create_function(|_, (old_workspace, new_workspace): (String, String)| {
            let mux = get_mux()?;
            mux.rename_workspace(&old_workspace, &new_workspace);
            Ok(())
        })?,
    )?;

    mux_mod.set(
        "get_window",
        lua.create_function(|_, window_id: WindowId| {
            let mux = get_mux()?;
            let window = MuxWindow(window_id);
            let _resolved = window.resolve(&mux)?;
            Ok(window)
        })?,
    )?;

    mux_mod.set(
        "get_pane",
        lua.create_function(|_, pane_id: PaneId| {
            let mux = get_mux()?;
            let pane = MuxPane(pane_id);
            pane.resolve(&mux)?;
            Ok(pane)
        })?,
    )?;

    mux_mod.set(
        "get_tab",
        lua.create_function(|_, tab_id: TabId| {
            let mux = get_mux()?;
            let tab = MuxTab(tab_id);
            tab.resolve(&mux)?;
            Ok(tab)
        })?,
    )?;

    mux_mod.set(
        "spawn_window",
        lua.create_async_function(|_, spawn: SpawnWindow| async move { spawn.spawn().await })?,
    )?;

    mux_mod.set(
        "all_windows",
        lua.create_function(|_, _: ()| {
            let mux = get_mux()?;
            Ok(mux
                .iter_windows()
                .into_iter()
                .map(MuxWindow)
                .collect::<Vec<MuxWindow>>())
        })?,
    )?;

    mux_mod.set(
        "get_domain",
        // Deliberately a *synchronous* lua function, like all_domains
        // below. It is reachable from synchronous callbacks
        // (format-tab-title, format-window-title,
        // augment-command-palette, mux-is-process-stateful), which
        // config::lua::emit_sync_callback invokes with a plain
        // `Function::call`. An mlua async function compiles down to a lua
        // wrapper that does `coroutine.yield` whenever the underlying
        // future returns Pending, so waiting for domain discovery in here
        // would make those callbacks fail outright with "attempt to yield
        // from outside a coroutine" -- and it would do so precisely
        // during the post-startup window this whole change introduces.
        //
        // So this is a snapshot lookup: while a background discovery is
        // still in flight, a domain it hasn't registered yet reads as
        // nil, the same way it does for all_domains. Spawning into a
        // domain *by name* does wait for discovery, though only up to
        // mux::DOMAIN_DISCOVERY_TIMEOUT (see Mux::resolve_spawn_tab_domain,
        // and that constant for why the bound is there and when it can
        // bite); it's only this accessor that doesn't wait at all.
        lua.create_function(|_, domain: LuaValue| {
            let mux = get_mux()?;
            match domain {
                LuaValue::Nil => Ok(Some(MuxDomain(mux.default_domain().domain_id()))),
                LuaValue::String(s) => match s.to_str() {
                    Ok(name) => Ok(mux
                        .get_domain_by_name(name)
                        .map(|dom| MuxDomain(dom.domain_id()))),
                    Err(err) => Err(mlua::Error::external(format!(
                        "invalid domain identifier passed to mux.get_domain: {err:#}"
                    ))),
                },
                LuaValue::Integer(id) => match TryInto::<DomainId>::try_into(id) {
                    Ok(id) => Ok(mux.get_domain(id).map(|dom| MuxDomain(dom.domain_id()))),
                    Err(err) => Err(mlua::Error::external(format!(
                        "invalid domain identifier passed to mux.get_domain: {err:#}"
                    ))),
                },
                _ => Err(mlua::Error::external(
                    "invalid domain identifier passed to mux.get_domain".to_string(),
                )),
            }
        })?,
    )?;

    mux_mod.set(
        "all_domains",
        lua.create_function(|_, _: ()| {
            // This is a snapshot of whatever domains are registered at
            // the moment of the call. If a background domain discovery
            // (eg. WSL) is still in flight -- which can be the case for
            // a few seconds right after startup -- domains it hasn't
            // found yet won't appear here. Like `mux.get_domain(name)`,
            // this is a synchronous snapshot that does NOT wait for
            // discovery. `wezterm.mux.spawn_window{domain=...}` does wait
            // (via `Mux::resolve_spawn_tab_domain` and
            // `await_domain_resolution_async`), but only up to
            // `mux::DOMAIN_DISCOVERY_TIMEOUT` -- a config that needs a
            // specific WSL domain guaranteed available this early should
            // set `wsl_domains` explicitly rather than rely on either.
            let mux = get_mux()?;
            Ok(mux
                .iter_domains()
                .into_iter()
                .map(|dom| MuxDomain(dom.domain_id()))
                .collect::<Vec<MuxDomain>>())
        })?,
    )?;

    mux_mod.set(
        "set_default_domain",
        lua.create_function(|_, domain: UserDataRef<MuxDomain>| {
            let mux = get_mux()?;
            let domain = domain.resolve(&mux)?;
            mux.set_default_domain(&domain);
            Ok(())
        })?,
    )?;

    Ok(())
}

#[derive(Debug, Default, FromDynamic, ToDynamic)]
struct CommandBuilderFrag {
    args: Option<Vec<String>>,
    cwd: Option<String>,
    #[dynamic(default)]
    set_environment_variables: HashMap<String, String>,
}

impl CommandBuilderFrag {
    fn to_command_builder(&self) -> (Option<CommandBuilder>, Option<String>) {
        if let Some(args) = &self.args {
            let mut builder = CommandBuilder::from_argv(args.iter().map(Into::into).collect());
            for (k, v) in self.set_environment_variables.iter() {
                builder.env(k, v);
            }
            if let Some(cwd) = self.cwd.clone() {
                builder.cwd(cwd);
            }
            (Some(builder), None)
        } else {
            (None, self.cwd.clone())
        }
    }
}

#[derive(Debug, FromDynamic, ToDynamic)]
enum HandySplitDirection {
    Left,
    Right,
    Top,
    Bottom,
}
impl_lua_conversion_dynamic!(HandySplitDirection);

impl Default for HandySplitDirection {
    fn default() -> Self {
        Self::Right
    }
}

#[derive(Debug, FromDynamic, ToDynamic)]
struct SpawnWindow {
    #[dynamic(default = "spawn_tab_default_domain")]
    domain: SpawnTabDomain,
    width: Option<usize>,
    height: Option<usize>,
    workspace: Option<String>,
    position: Option<config::GuiPosition>,
    #[dynamic(flatten)]
    cmd_builder: CommandBuilderFrag,
}
impl_lua_conversion_dynamic!(SpawnWindow);

fn spawn_tab_default_domain() -> SpawnTabDomain {
    SpawnTabDomain::DefaultDomain
}

impl SpawnWindow {
    async fn spawn(self) -> mlua::Result<(MuxTab, MuxPane, MuxWindow)> {
        let mux = get_mux()?;

        let size = match (self.width, self.height) {
            (Some(cols), Some(rows)) => TerminalSize {
                rows,
                cols,
                ..Default::default()
            },
            _ => config::configuration().initial_size(0, None),
        };

        let (cmd_builder, cwd) = self.cmd_builder.to_command_builder();
        let (tab, pane, window_id) = mux
            .spawn_tab_or_window(
                None,
                self.domain,
                cmd_builder,
                cwd,
                size,
                None,
                self.workspace.unwrap_or_else(|| mux.active_workspace()),
                self.position,
            )
            .await
            .map_err(|e| mlua::Error::external(format!("{:#?}", e)))?;

        Ok((
            MuxTab(tab.tab_id()),
            MuxPane(pane.pane_id()),
            MuxWindow(window_id),
        ))
    }
}

#[derive(Debug, FromDynamic, ToDynamic)]
struct SpawnTab {
    #[dynamic(default)]
    domain: SpawnTabDomain,
    #[dynamic(flatten)]
    cmd_builder: CommandBuilderFrag,
}
impl_lua_conversion_dynamic!(SpawnTab);

impl SpawnTab {
    async fn spawn(self, window: &MuxWindow) -> mlua::Result<(MuxTab, MuxPane, MuxWindow)> {
        let mux = get_mux()?;
        let size;
        let pane;

        {
            let window = window.resolve(&mux)?;
            size = window
                .get_by_idx(0)
                .map(|tab| tab.get_size())
                .unwrap_or_else(|| config::configuration().initial_size(0, None));

            pane = window
                .get_active()
                .and_then(|tab| tab.get_active_pane().map(|pane| pane.pane_id()));
        };

        let (cmd_builder, cwd) = self.cmd_builder.to_command_builder();

        let (tab, pane, window_id) = mux
            .spawn_tab_or_window(
                Some(window.0),
                self.domain,
                cmd_builder,
                cwd,
                size,
                pane,
                String::new(),
                None, // optional gui window position
            )
            .await
            .map_err(|e| mlua::Error::external(format!("{:#?}", e)))?;

        Ok((
            MuxTab(tab.tab_id()),
            MuxPane(pane.pane_id()),
            MuxWindow(window_id),
        ))
    }
}

#[derive(Clone, FromDynamic, ToDynamic)]
struct MuxTabInfo {
    pub index: usize,
    pub is_active: bool,
}
impl_lua_conversion_dynamic!(MuxTabInfo);

#[derive(Clone, FromDynamic, ToDynamic)]
struct MuxPaneInfo {
    /// The topological pane index that can be used to reference this pane
    pub index: usize,
    /// true if this is the active pane at the time the position was computed
    pub is_active: bool,
    /// true if this pane is zoomed
    pub is_zoomed: bool,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this pane, in cells.
    pub left: usize,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this pane, in cells.
    pub top: usize,
    /// The width of this pane in cells
    pub width: usize,
    pub pixel_width: usize,
    /// The height of this pane in cells
    pub height: usize,
    pub pixel_height: usize,
}
impl_lua_conversion_dynamic!(MuxPaneInfo);
