# `wezterm.url.encode(string)`

{{since('nightly')}}

Returns the percent encoded version of the provided string.

## Example: opening a line in browser with a search engine

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

local config = wezterm.config_builder()

local search_engine_prefixes = {
  ['Google'] = 'https://www.google.com/search?q=',
  ['Youtube'] = 'https://www.youtube.com/results?search_query=',
}

local function open_line_with_search_engine(state)
  return wezterm.action_callback(function(window, pane, line)
    if line and #line > 0 then
      wezterm.open_with(
        search_engine_prefixes[state.search_engine]
          .. wezterm.url.encode(line)
      )
    end
  end)
end

local function prompt_for_line(state)
  return act.PromptInputLine {
    description = state.search_engine,
    action = open_line_with_search_engine(state),
  }
end

config.keys = {
  {
    key = 'g',
    mods = 'CTRL',
    action = wezterm.action_callback(function(window, pane)
      local state = {}
      state.search_engine = 'Google'
      window:perform_action(prompt_for_line(state), pane)
    end),
  },
  {
    key = 'y',
    mods = 'CTRL',
    action = wezterm.action_callback(function(window, pane)
      local state = {}
      state.search_engine = 'Youtube'
      window:perform_action(prompt_for_line(state), pane)
    end),
  },
}

return config
```
