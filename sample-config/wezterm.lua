-- WezTerm configuration migrated from Terminator (~/.config/terminator/config)
local wezterm = require 'wezterm'
local act = wezterm.action

local config = wezterm.config_builder()

--------------------------------------------------------------------------------
-- Terminal & Scrollback
--------------------------------------------------------------------------------
-- Terminator: term = 'xterm-256color'
config.term = 'xterm-256color'

-- Terminator: scrollback_infinite = True (scrollback_lines = 0)
-- WezTerm does not support infinite scrollback; using a large buffer instead.
config.scrollback_lines = 100000

-- Terminator: scroll_on_keystroke = True, scroll_on_output = False
config.scroll_to_bottom_on_input = true

-- Terminator: exit_action = 'close'
config.exit_behavior = 'Close'

-- Terminator: audible_bell = False, visible_bell = False
config.audible_bell = 'Disabled'

--------------------------------------------------------------------------------
-- Font & Text
--------------------------------------------------------------------------------
-- Terminator: use_system_font = True -> system monospace font: 'DejaVu Sans Mono 10'
config.font = wezterm.font 'DejaVu Sans Mono'
config.font_size = 10.0

-- Terminator: cell_width = 1.0, cell_height = 1.0
config.cell_width = 1.0
config.line_height = 1.0

-- Terminator: bold_is_bright = False
config.bold_brightens_ansi_colors = 'No'

--------------------------------------------------------------------------------
-- Colors & Appearance
--------------------------------------------------------------------------------
-- Terminator defaults (use_theme_colors = False): black bg, #aaaaaa fg, Tango palette
config.colors = {
  foreground = '#aaaaaa',
  background = '#000000',
  cursor_bg = '#aaaaaa',
  cursor_fg = '#000000',
  cursor_border = '#aaaaaa',
  split = '#808080',
  ansi = {
    '#2e3436', -- black
    '#cc0000', -- red
    '#4e9a06', -- green
    '#c4a000', -- yellow
    '#3465a4', -- blue
    '#75507b', -- magenta
    '#06989a', -- cyan
    '#d3d7cf', -- white
  },
  brights = {
    '#555753', -- bright black
    '#ef2929', -- bright red
    '#8ae234', -- bright green
    '#fce94f', -- bright yellow
    '#729fcf', -- bright blue
    '#ad7fa8', -- bright magenta
    '#34e2e2', -- bright cyan
    '#eeeeec', -- bright white
  },
}

-- Terminator: inactive_color_offset = 0.8
config.inactive_pane_hsb = {
  saturation = 1.0,
  brightness = 0.8,
}

-- Split pane divider thickness (default for DejaVu Sans Mono 10pt is ~0.59px -> 1px; +3px = 4px)
config.underline_thickness = '4px'

-- Terminator: cursor_shape = 'block', cursor_blink = True
config.default_cursor_style = 'BlinkingBlock'

-- Terminator: scrollbar_position = 'right'
config.enable_scroll_bar = true

-- Terminator: tab_position = 'top', only shown when multiple tabs exist
config.tab_bar_at_bottom = false
config.hide_tab_bar_if_only_one_tab = true

-- Terminator: borderless = False, ask_before_closing = 'multiple_terminals'
config.window_decorations = 'TITLE | RESIZE'
config.window_close_confirmation = 'AlwaysPrompt'

-- Terminator: mouse_autohide = True
config.hide_mouse_cursor_when_typing = true

--------------------------------------------------------------------------------
-- Helper Callbacks for Terminator-specific Actions
--------------------------------------------------------------------------------
-- Terminator: split_auto (Ctrl+Shift+A) - split along the longer dimension
local function split_auto(window, pane)
  local dims = pane:get_dimensions()
  if dims.pixel_width >= dims.pixel_height then
    window:perform_action(act.SplitHorizontal { domain = 'CurrentPaneDomain' }, pane)
  else
    window:perform_action(act.SplitVertical { domain = 'CurrentPaneDomain' }, pane)
  end
end

-- Terminator: toggle_scrollbar (Ctrl+Shift+S)
local function toggle_scrollbar(window, _pane)
  local overrides = window:get_config_overrides() or {}
  if overrides.enable_scroll_bar == nil then
    overrides.enable_scroll_bar = not config.enable_scroll_bar
  else
    overrides.enable_scroll_bar = not overrides.enable_scroll_bar
  end
  window:set_config_overrides(overrides)
end

--------------------------------------------------------------------------------
-- Keybindings (matched to Terminator)
--------------------------------------------------------------------------------
config.keys = {
  -- Pane splitting
  -- Note: Terminator's "horizontal split" (Ctrl+Shift+O) puts panes top/bottom
  -- (WezTerm's SplitVertical), and "vertical split" (Ctrl+Shift+E) puts panes
  -- left/right (WezTerm's SplitHorizontal).
  { key = 'O', mods = 'CTRL|SHIFT', action = act.SplitVertical { domain = 'CurrentPaneDomain' } },
  { key = 'E', mods = 'CTRL|SHIFT', action = act.SplitHorizontal { domain = 'CurrentPaneDomain' } },
  { key = 'A', mods = 'CTRL|SHIFT', action = wezterm.action_callback(split_auto) },

  -- Pane navigation
  { key = 'LeftArrow', mods = 'ALT', action = act.ActivatePaneDirection 'Left' },
  { key = 'RightArrow', mods = 'ALT', action = act.ActivatePaneDirection 'Right' },
  { key = 'UpArrow', mods = 'ALT', action = act.ActivatePaneDirection 'Up' },
  { key = 'DownArrow', mods = 'ALT', action = act.ActivatePaneDirection 'Down' },
  { key = 'N', mods = 'CTRL|SHIFT', action = act.ActivatePaneDirection 'Next' },
  { key = 'P', mods = 'CTRL|SHIFT', action = act.ActivatePaneDirection 'Prev' },
  { key = 'Tab', mods = 'CTRL', action = act.ActivatePaneDirection 'Next' },
  { key = 'Tab', mods = 'CTRL|SHIFT', action = act.ActivatePaneDirection 'Prev' },

  -- Pane resizing
  { key = 'LeftArrow', mods = 'CTRL|SHIFT', action = act.AdjustPaneSize { 'Left', 5 } },
  { key = 'RightArrow', mods = 'CTRL|SHIFT', action = act.AdjustPaneSize { 'Right', 5 } },
  { key = 'UpArrow', mods = 'CTRL|SHIFT', action = act.AdjustPaneSize { 'Up', 5 } },
  { key = 'DownArrow', mods = 'CTRL|SHIFT', action = act.AdjustPaneSize { 'Down', 5 } },

  -- Pane zoom & rotation
  { key = 'X', mods = 'CTRL|SHIFT', action = act.TogglePaneZoomState },
  { key = 'Z', mods = 'CTRL|SHIFT', action = act.TogglePaneZoomState },
  { key = 'r', mods = 'SUPER', action = act.RotatePanes 'Clockwise' },
  { key = 'R', mods = 'SUPER|SHIFT', action = act.RotatePanes 'CounterClockwise' },

  -- Closing pane / window
  { key = 'W', mods = 'CTRL|SHIFT', action = act.CloseCurrentPane { confirm = false } },
  { key = 'Q', mods = 'CTRL|SHIFT', action = act.QuitApplication },

  -- Tabs
  { key = 'T', mods = 'CTRL|SHIFT', action = act.SpawnTab 'CurrentPaneDomain' },
  { key = 'PageDown', mods = 'CTRL', action = act.ActivateTabRelative(1) },
  { key = 'PageUp', mods = 'CTRL', action = act.ActivateTabRelative(-1) },
  { key = 'PageDown', mods = 'CTRL|SHIFT', action = act.MoveTabRelative(1) },
  { key = 'PageUp', mods = 'CTRL|SHIFT', action = act.MoveTabRelative(-1) },
  {
    key = 'a',
    mods = 'CTRL|ALT',
    action = act.PromptInputLine {
      description = 'Enter new name for tab',
      action = wezterm.action_callback(function(window, _pane, line)
        if line then
          window:active_tab():set_title(line)
        end
      end),
    },
  },

  -- Windows & fullscreen
  { key = 'I', mods = 'CTRL|SHIFT', action = act.SpawnWindow },
  { key = 'i', mods = 'SUPER', action = act.SpawnWindow },
  { key = 'F11', mods = 'NONE', action = act.ToggleFullScreen },

  -- Copy / paste / search (case_sensitive = False in Terminator config)
  { key = 'C', mods = 'CTRL|SHIFT', action = act.CopyTo 'Clipboard' },
  { key = 'V', mods = 'CTRL|SHIFT', action = act.PasteFrom 'Clipboard' },
  { key = 'F', mods = 'CTRL|SHIFT', action = act.Search { CaseInSensitiveString = '' } },

  -- Font zoom
  { key = '+', mods = 'CTRL', action = act.IncreaseFontSize },
  { key = '=', mods = 'CTRL', action = act.IncreaseFontSize },
  { key = '-', mods = 'CTRL', action = act.DecreaseFontSize },
  { key = '0', mods = 'CTRL', action = act.ResetFontSize },

  -- Terminal reset & clear
  { key = 'R', mods = 'CTRL|SHIFT', action = act.ResetTerminal },
  {
    key = 'G',
    mods = 'CTRL|SHIFT',
    action = act.Multiple {
      act.ResetTerminal,
      act.ClearScrollback 'ScrollbackAndViewport',
    },
  },

  -- Scrollback navigation
  { key = 'Home', mods = 'CTRL|SHIFT', action = act.ScrollToTop },
  { key = 'End', mods = 'CTRL|SHIFT', action = act.ScrollToBottom },

  -- Toggle scrollbar
  { key = 'S', mods = 'CTRL|SHIFT', action = wezterm.action_callback(toggle_scrollbar) },
}

return config
