# Bundled resurrect.wezterm

This crate compiles the Lua sources of the
[resurrect.wezterm](https://github.com/MLFlexer/resurrect.wezterm) plugin
(MIT licensed, see [LICENSE](LICENSE)) into wezterm, so that it can be used
without fetching anything from the network:

```lua
local resurrect = require("resurrect")
```

Do not also load the upstream plugin via `wezterm.plugin.require`: the
bundled modules take precedence over any other `resurrect.*` modules.

## Provenance

`src/lua/resurrect/*.lua` were imported from upstream commit
`65cbbbf6d2c76f3e36af7610a356fc190fcb6147` (mirrored at
<https://github.com/dbevdev/resurrect.wezterm>); `git log -p` on that
directory shows the unmodified import followed by the changes listed below.
`src/lua/init.lua` replaces the upstream `plugin/init.lua`.

## Differences from upstream

* No runtime dependency on the third party `dev.wezterm` plugin, which the
  upstream entry point downloads from GitHub to locate its own checkout.
* The state is saved in `<wezterm data dir>/resurrect/` (for example
  `%LOCALAPPDATA%\wezterm\resurrect\` on Windows) rather than inside the
  plugin checkout. Use `resurrect.state_manager.change_state_save_dir(dir)`
  to change it; `dir` must end with a path separator.
* No external processes are spawned, except for the optional encryption
  (`age`, `rage` or `gpg`) which you have to enable explicitly:
  * creating the state directories uses a native helper instead of `mkdir`
    (the upstream Windows command was also malformed);
  * listing the saved states for the fuzzy loader uses a native helper
    instead of a VBS script run via `wscript.exe` on Windows, or
    `find`/`stat`/`awk` elsewhere.
* Saving a tab works with dividers wider than one cell
  (`pane_divider_cols` / `pane_divider_rows`); upstream assumed that
  adjacent panes are exactly one cell apart and silently dropped panes.
* `save_tab_action()` / `save_window_action()` no longer call the
  nonexistent `resurrect.save_state` after prompting for a title.

## Example

```lua
local wezterm = require("wezterm")
local resurrect = require("resurrect")
local config = wezterm.config_builder()

-- Save the current workspace every 15 minutes, and remember it so that
-- it can be restored when wezterm is started
resurrect.state_manager.periodic_save({ save_workspaces = true })
wezterm.on("resurrect.state_manager.periodic_save.finished", function()
  resurrect.state_manager.write_current_state(wezterm.mux.get_active_workspace(), "workspace")
end)
wezterm.on("gui-startup", resurrect.state_manager.resurrect_on_gui_startup)

config.keys = {
  -- Save the current workspace
  {
    key = "w",
    mods = "ALT",
    action = wezterm.action_callback(function(win, pane)
      local state = resurrect.workspace_state.get_workspace_state()
      resurrect.state_manager.save_state(state)
      resurrect.state_manager.write_current_state(state.workspace, "workspace")
    end),
  },
  -- Save the current window / tab (prompts for a title if there is none)
  { key = "W", mods = "ALT", action = resurrect.window_state.save_window_action() },
  { key = "T", mods = "ALT", action = resurrect.tab_state.save_tab_action() },
  -- Pick a saved workspace, window or tab and restore it
  {
    key = "r",
    mods = "ALT",
    action = wezterm.action_callback(function(win, pane)
      resurrect.fuzzy_loader.fuzzy_load(win, pane, function(id, label)
        -- id is "<type><path separator><name>.json"
        local type, name = id:match("^([^/\\]+)[/\\](.+)%.json$")
        local opts = {
          relative = true,
          restore_text = true,
          on_pane_restore = resurrect.tab_state.default_on_pane_restore,
        }
        local state = resurrect.state_manager.load_state(name, type)
        if type == "workspace" then
          resurrect.workspace_state.restore_workspace(state, opts)
        elseif type == "window" then
          resurrect.window_state.restore_window(pane:window(), state, opts)
        elseif type == "tab" then
          resurrect.tab_state.restore_tab(pane:tab(), state, opts)
        end
      end)
    end),
  },
}

return config
```

See the [upstream README](https://github.com/MLFlexer/resurrect.wezterm#readme)
for the remaining options (encryption, `restore_opts`, events).

## Security notes

* The saved state contains up to 3500 lines of scrollback per pane
  (see `resurrect.state_manager.set_max_nlines`) in plain text unless
  encryption is enabled, so it may contain secrets that were displayed in
  the terminal.
* `default_on_pane_restore` re-runs the saved command line of panes that
  were showing a full screen application (for example an editor), so only
  restore state files that you trust.
