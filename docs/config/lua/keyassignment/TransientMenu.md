# `TransientMenu`

{{since('nightly')}}

This creates an overlay with keyboard driven menu similar to
Emacs transient menus. 

We can view and set switches, options, cyclic switches and
select an argument to trigger action with the current state
of above-mentioned entities passed as an argument.


`TransientMenu` accepts the following fields:

* `description` - text to display at the top of the menu
* `context` - an optional argument that accepts a [TransientContext](../TransientContext.md)
  object
* `sections` - list of [TransientSection](#transientsection) objects
* `cancel` - event callback registered via `wezterm.action_callback`. The
  callback's function signature is `(window, pane)` where `window` and
  `pane` are the [Window](../window/index.md) and [Pane](../pane/index.md).
  This is an optional argument. If present, this callback is called when the
  user cancels the current overlay


### `TransientSection`

`TransientSection` struct is a lua object with the following fields:
* `header` - text to describe the section
* `entries` - list of [TransientEntry](#transiententry) objects


### `TransientEntry`

`TransientEntry` struct is a lua object for which the possible values are:
* `{ TransientSwitch = obj }` where `obj` is a [TransientSwitch](../TransientSwitch.md)
  object
* `{ TransientOption = obj }` where `obj` is a [TransientOption](../TransientOption.md)
  object
* `{ TransientCyclicSwitch = obj }` where `obj` is a
  [TransientCyclicSwitch](../TransientCyclicSwitch.md) object
* `{ TransientArgument = obj }` where `obj` is a [TransientArgument](../TransientArgument.md)
  object

## Combining TransientMenu and SelectorActions for viewing logs for Docker containers with an ability to move between KeyAssignments

{% raw %}
```lua
local wezterm = require 'wezterm'
local act = wezterm.action

local function description(text)
  return wezterm.format {
    { Attribute = { Intensity = 'Bold' } },
    { Foreground = { AnsiColor = 'Teal' } },
    { Text = text },
  }
end

local function header(text)
  return wezterm.format {
    { Attribute = { Intensity = 'Bold' } },
    { Foreground = { AnsiColor = 'Navy' } },
    { Text = text },
  }
end

local function entry_label(text)
  return wezterm.format {
    { Foreground = { AnsiColor = 'Olive' } },
    { Text = text },
  }
end

local function fuzzy_description(text)
  return wezterm.format {
    { Attribute = { Intensity = 'Bold' } },
    { Foreground = { AnsiColor = 'Teal' } },
    { Text = text },
    'ResetAttributes',
    { Text = ': ' },
  }
end

local docker_actions_transient
local containers_selector_actions
local containers_logs_transient

containers_logs_transient = function(state)
  return wezterm.action_callback(function(window, pane)
    local selected_containers = {}
    for _, container in ipairs(state.choices) do
      table.insert(selected_containers, container.label)
    end

    window:perform_action(
      act.TransientMenu {
        description = description 'Docker container logs',
        context = {
          header = header 'Context',
          entries = {
            {
              label = entry_label 'Entity',
              id = 'Containers',
            },
            {
              label = entry_label 'Operation',
              id = 'Logs',
            },
            {
              label = entry_label 'Selected containers',
              id = table.concat(selected_containers, ', '),
            },
          },
        },
        sections = {
          {
            header = header 'Flags',
            entries = {
              {
                TransientSwitch = {
                  key = '-f',
                  default = true,
                  description = 'Follow',
                  flag = '--follow',
                },
              },
              {
                TransientOption = {
                  key = '-t',
                  default = '0',
                  description = 'Tail',
                  flag = '--tail=',
                  allow_nil = false,
                },
              },
            },
          },
          {
            header = header 'Actions',
            entries = {
              {
                TransientArgument = {
                  key = 'l',
                  description = 'Logs',
                  action = wezterm.action_callback(
                    function(inner_window, inner_pane, result)
                      local cmd = { 'docker', 'logs' }
                      for _, entry in ipairs(result.entries) do
                        if entry.value == true then
                          table.insert(cmd, entry.flag)
                        elseif entry.value then
                          table.insert(cmd, entry.flag .. entry.value)
                        end
                      end

                      local cmd_len = #cmd
                      for _, container in ipairs(state.choices) do
                        cmd[cmd_len + 1] = container.id
                        inner_window:perform_action(
                          act.SpawnCommandInNewTab { args = cmd },
                          inner_pane
                        )
                      end
                    end
                  ),
                },
              },
            },
          },
        },
        cancel = wezterm.action_callback(function(inner_window, inner_pane)
          state.choices = nil
          inner_window:perform_action(
            containers_selector_actions(state),
            inner_pane
          )
        end),
      },
      pane
    )
  end)
end

containers_selector_actions = function(state)
  return wezterm.action_callback(function(window, pane)
    local success, stdout, stderr = wezterm.run_child_process {
      'docker',
      'container',
      'ls',
      '--format',
      '{{.ID}}:{{.Names}}',
    }
    if success then
      local containers = {}
      for _, line in ipairs(wezterm.split_by_newlines(stdout)) do
        local id, name = line:match '(.-):(.+)'
        if id and name then
          table.insert(containers, { label = name, id = id })
        end
      end

      window:perform_action(
        act.SelectorActions {
          description = description 'Select containers',
          context = {
            header = header 'Context',
            entries = {
              {
                label = entry_label 'Entity',
                id = 'Containers',
              },
            },
          },
          choices = containers,
          section = {
            header = header 'Actions',
            arguments = {
              {
                key = 'l',
                description = 'Logs',
                action = wezterm.action_callback(
                  function(inner_window, inner_pane, result)
                    state.choices = result.choices

                    inner_window:perform_action(
                      containers_logs_transient(state),
                      inner_pane
                    )
                  end
                ),
              },
            },
          },
          fuzzy_description = fuzzy_description 'Select containers',
          multiple = true,
          cancel = wezterm.action_callback(function(inner_window, inner_pane)
            inner_window:perform_action(
              docker_actions_transient(state),
              inner_pane
            )
          end),
        },
        pane
      )
    end
  end)
end

docker_actions_transient = function(state)
  return wezterm.action_callback(function(window, pane)
    window:perform_action(
      act.TransientMenu {
        description = description 'Docker action',
        sections = {
          {
            header = header 'Actions',
            entries = {
              {
                TransientArgument = {
                  key = 'c',
                  description = 'Containers',
                  action = wezterm.action_callback(
                    function(inner_window, inner_pane, result)
                      inner_window:perform_action(
                        containers_selector_actions(state),
                        inner_pane
                      )
                    end
                  ),
                },
              },
            },
          },
        },
      },
      pane
    )
  end)
end

local config = wezterm.config_builder()

config.keys = {
  {
    key = 'k',
    mods = 'CTRL',
    action = wezterm.action_callback(function(window, pane)
      local state = {}
      window:perform_action(docker_actions_transient(state), pane)
    end),
  },
}

return config
```
{% endraw %}
