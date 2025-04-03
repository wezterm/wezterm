# `user-dropped-{strings,paths,urls}`

{{since('nightly')}}

```lua
local wezterm = require 'wezterm'

wezterm.on('user-dropped-strings', function(window, pane, strings) end)

wezterm.on('user-dropped-paths', function(window, pane, paths) end)

wezterm.on('user-dropped-urls', function(window, pane, urls) end)
```

The parameters to the event are:

- `window` - the [Window](../window/index.md) for the active tab
- `pane` - the target [Pane](../pane/index.md) the content was dropped
- `strings` - an array containing the strings dropped into the pane
  (user-dropped-strings)
- `paths` - an array containing the paths dropped into the pane and already
  quoted if the
  [`quote_dropped_files` config option](../config/quote_dropped_files.md) is set
  (user-dropped-paths)
- `urls` - an array containing the urls dropped into the pane
  (user-dropped-urls)

The return value of the event can be:

- a non-empty string, the returned string contents will be `send_paste`'d into
  the target pane
- an empty string `return ""`, which will abort sending anything to the pane
- `nil`, will perform the default action of joining the content by a space, and
  pasting the entries into the target pane

## `user-dropped-paths`

The `user-dropped-paths` event is emitted when the user drops file objects from
a windowed file explorer to a pane.

The default action is to quote the full paths as defined by the
[`quote_dropped_files` config option](../config/quote_dropped_files.md), then
join them all together separated by a space, and insert them into the pane they
were dropped into. If you register for this event however, you can handle the
items however you see fit.

The simplest use case is if you wish make your config file more portable and use
the quote_path function yourself so that a Windows and Linux system can share
the same configuration file. One may also choose to send_text instead of
send_paste.

```lua
local wezterm = require 'wezterm'

wezterm.on('user-dropped-paths', function(window, pane, paths)
  local quoted = {}
  -- Log the dropped paths for debugging

  for _, path in ipairs(paths) do
    wezterm.log_info('Dropped path:', path)
    -- Quote the path based on the current OS
    if wezterm.hostname() == 'linuxhost' then
      local quoted_path = wezterm.filesystem.quote_path(path, 'Posix') -- Use Posix for cross-platform compatibility
    else
      local quoted_path = wezterm.filesystem.quote_path(path, 'Windows') -- Use Windows quoting for Windows systems
    end
    table.insert(quoted, quoted_path)
  end

  -- Default behavior is to insert the paths into the pane with paste
  local output = table.concat(quoted, ' ') -- Join paths with a space
  pane:send_text(output) -- Send the text to the pane instead of pasting

  return '' -- Override default behavior that would send paste
end)
```

For example, if you prefer to handle them differently based on certain criteria,
such as the current foreground app not readily supporting strings of file paths
being pasted in when running.

One such example could be neovim or some other editor that the default insertion
of the paths could only be useful for inserting them into a document or text
file or the when a command has been started and the paths will complete it's
arguments.

One case of handling the paths differently if neovim is the foreground process,
is that it opens the files dropped. While this example may not work for
everyone, it should provide an idea of how to handle customizing the event's
reactions.

```lua
local wezterm = require 'wezterm'

-- default nvim socket path for the current user and pid
-- this is not the same for all distro and users and for
-- hardcoded --listen arguments
local nvim_socket_path = '/run/user/%d/nvim.%d.0'

wezterm.on('user-dropped-paths', function(window, pane, paths)
  local fg = pane:get_foreground_process_info()
  if fg and fg.name == 'nvim' then
    -- Get the current user id and the nvim pid to search for the socket
    local _, id, _ = wezterm.run_child_process { 'id', '-u' }

    -- Get the nvim pid from the foreground process info, but not the top level parent process, the first fork
    local pid = 0
    for _pid, _ in pairs(fg.children) do
      pid = _pid
      break
    end

    wezterm.log_info(
      string.format(
        'nvim process detected, checking for default socket %s',
        string.format(nvim_socket_path, id, pid)
      )
    )

    -- check if we can use the socket or fall back to injecting neovim shortcuts
    if
      wezterm.filesystem.exists(string.format(nvim_socket_path, id, pid))
    then
      wezterm.log_info 'nvim remote socket found, opening files in new nvim instance'
      -- Use the default nvim socket instance to open the files
      for _, path in ipairs(paths) do
        -- check if the path is a file
        if wezterm.filesystem.is_file(path) then
          wezterm.log_info('Opening file:', path)

          local nvim_command = {
            'nvim', -- The command to run
            '--server', -- The server socket path
            string.format(nvim_socket_path, id, pid), -- The socket path
            '--remote', -- The remote command
            path, -- The file to open
          }
          wezterm.background_child_process(nvim_command)
        else
          wezterm.log_info('Path is not a file, skipping:', path)
        end
      end
    else
      wezterm.log_info 'No nvim remote socket fall back to doing neovim motions for escaping to normal mode, then activate insert mode with the dropped paths'
      local output = '\x1b' -- Escape sequence to enter normal mode
        .. ':arge' -- Use cmdline argedit cmd to open multiple files for editing
        .. ' ' -- separate arguments
        .. table.concat(paths, ' ') -- Concatenate paths with spaces
        .. '\r' -- Carriage return to execute the command
      wezterm.log_info('Sendding:', output) -- Log the output
      pane:send_text(output) -- Send the command to the pane without pasting
      return '' -- Override default behavior
    end
  end
  -- if we dont return anything then it's the same
  -- as returning nil and default action will run
end)
```

### user-dropped-urls

When the content of a drag and drop is determined to be a URL then this event
will be emitted with an array of all the URLs that were dropped.

One way to customize your handling of URLs being dropped into a pane is to
determine if they are git repo's with a .git extension, and immediately invoking
a cloning of it within the current directory.

```lua
local wezterm = require 'wezterm'

wezterm.on('user-dropped-urls', function(window, pane, urls)
  for _, url in ipairs(urls) do
    -- Handle git repository URLs specially
    if url:match '%.git$' then
      -- Create a command to clone the git repository and inject
      -- into the beginning of current prompt as to not destroy it
      local cmd_string = '\1\1' -- "\1" to send ctrl-a (usually 'move to beginning of line', doubled up in case of tmux prefix)
        .. 'git clone --depth 1' -- The git command to clone shallow
        .. url -- The repository URL
        .. ' ' -- We toss a space in the end to
        .. '\r' -- Carriage return to execute the command

      wezterm.log_info('Git URL detected:', url)
      wezterm.log_info('Command string:', cmd_string)

      -- Create an action callback to send the command to the pane
      local action = wezterm.action_callback(function(_wid, pid, cmd_string)
        local p = wezterm.mux.get_pane(pid)
        p:send_text(cmd_string)
      end)

      -- Emit the event to trigger the callback
      wezterm.emit(
        action.EmitEvent,
        window:window_id(),
        pane:pane_id(),
        cmd_string
      )
      return '' -- Override default behavior
    end
  end
end)
```

## Use modifier keys to cause specific actions

```lua
local wezterm = require 'wezterm'

wezterm.on('user-dropped-paths', function(window, pane, paths)
  local mods = window:keyboard_modifiers()
  wezterm.log_info('Keyboard modifiers:', inspect(pane))

  -- Handle ALT modifier key for file copying
  if mods:find 'ALT' then
    wezterm.log_info 'ALT key held down, checking files'
    local cwd = pane:get_current_working_dir().path
    wezterm.log_info('Current working directory:', cwd)

    -- Create a callback to copy files to the current directory
    local copier_event = wezterm.action_callback(function(_, _, ...)
      -- Helper function to extract the basename from a path
      function basename(s)
        return string.gsub(s, '(.*[/\\])(.*)', '%2')
      end

      for _, drop in ipairs(drops) do
        -- Check if the dropped file is outside the current working directory
        local ok, stdo, stde = wezterm.run_child_process {
          'realpath',
          '--relative-to',
          cwd,
          wezterm.filesystem.quote_path(drop, 'Posix'),
        }

        -- If path contains ".." it's outside the current directory, so copy it
        if ok and string.find(stdo, '..', 1, true) then
          wezterm.log_info('Copying', drop, 'to', cwd)
          wezterm.background_child_process { 'cp', '-r', drop, cwd }
        end
      end
    end)

    -- Emit the event to trigger the callback
    wezterm.emit(copier_event.EmitEvent)
    return '' -- Override default behavior
  end
end)
```
