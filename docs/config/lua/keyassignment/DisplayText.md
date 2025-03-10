# `DisplayText`

{{since('nightly')}}

Activates an overlay to display the provided text.

`DisplayText` accepts the following fields:

* `text` - the content to display in the overlay. You may embed
  escape sequences and/or use [wezterm.format](../wezterm/format.md)

### Key Assignments

The default key assignments in DisplayText are as follows:

| Action  |  Key Assignment |
|---------|-------------------|
| Quit     | <kbd>Ctrl</kbd> + <kbd>G</kbd> |
|          | <kbd>Ctrl</kbd> + <kbd>C</kbd> |
|          | <kbd>Ctrl</kbd> + <kbd>D</kbd> |
|          | <kbd>Ctrl</kbd> + <kbd>&#91;</kbd> |
|          | <kbd>Escape</kbd> |

## Example of displaying the status of wezterm run_child_process

```lua
local wezterm = require 'wezterm'
local act = wezterm.action
local config = wezterm.config_builder()

config.keys = {
  {
    key = 'E',
    mods = 'CTRL|SHIFT',
    action = wezterm.action_callback(function(window, pane)
      local success, stdout, stderr = wezterm.run_child_process {
        'docker',
        'container',
        'ls',
      }
      local display_text = success and stdout or stderr
      window:perform_action(act.DisplayText { text = display_text }, pane)
    end),
  },
}

return config
```
