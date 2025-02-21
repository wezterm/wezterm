# `Confirmation`

{{since('nightly')}}

Activates an overlay to display a confirmation menu

When the user accepts a line, emits an event that allows you to act
upon the input.

`Confirmation` accepts the following fields:

* `message` - the text to show for confirmation. You may embed
  escape sequences and/or use [wezterm.format](../wezterm/format.md).
  Defaults to: `"🛑 Really continue?"`.

## Example of choosing a program with user confirmation

```lua
local wezterm = require 'wezterm'
local act = wezterm.action
local config = wezterm.config_builder()

config.keys = {
  {
    key = 'E',
    mods = 'CTRL|SHIFT',
    action = act.Confirmation {
      message = "Do you want to run htop in a new window?",
      action = wezterm.action_callback(function(window, pane)
        window:perform_action(act.SpawnCommandInNewWindow { args = { 'htop' } }, pane)
      end),
    },
  },
}

return config
```




See also [InputSelector](InputSelector.md).
See also [PromptInputLine](PromptInputLine.md).
