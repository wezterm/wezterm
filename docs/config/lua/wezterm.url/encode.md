# `wezterm.url.encode(string)`

{{since('nightly')}}

Returns the percent encoded version of the provided string.

## Example: opening a line in browser with a search engine

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

local config = wezterm.config_builder()

local SEARCH_ENGINES = {
  ['Google'] = 'https://www.google.com/search?q=',
  ['Youtube'] = 'https://www.youtube.com/results?search_query=',
}

local function get_action_to_search(search_engine)
  return act.PromptInputLine {
    description = 'Search with ' .. search_engine,
    action = wezterm.action_callback(function(_win, _pane, line)
      if line and #line > 0 then
        local final_url = SEARCH_ENGINES[search_engine] .. wezterm.url.encode(line)
        wezterm.open_with(final_url)
      end
    end),
  }
end

config.keys = {
  {
    key = 'g',
    mods = 'CTRL',
    action = get_action_to_search('Google'),
  },
  {
    key = 'y',
    mods = 'CTRL',
    action = get_action_to_search('Youtube'),
  },
}

return config
```
