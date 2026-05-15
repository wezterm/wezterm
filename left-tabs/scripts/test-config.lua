-- Minimal config for the left-tabs test instance.
-- Isolates from Kai's daily wezterm session: own runtime dir, no
-- mux domain, unique window class so screencapture can find it.

local wezterm = require 'wezterm'

local config = {}

config.font_size = 14.0
config.enable_tab_bar = true
config.use_fancy_tab_bar = true
config.hide_tab_bar_if_only_one_tab = false
config.color_scheme = 'Tokyo Night'
config.tab_bar_position = 'Left'

config.window_decorations = 'TITLE | RESIZE'

-- Avoid touching Kai's mux runtime.
config.unix_domains = {}
config.default_prog = { '/bin/zsh', '-l' }

config.initial_cols = 100
config.initial_rows = 28

return config
