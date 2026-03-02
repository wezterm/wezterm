---
title: wezterm.filesystem
---

# `wezterm.filesystem`

This module provides utility functions for filesystem operations. Below are the available functions with examples for each.

## `canonicalize_path(path)`

This function returns the canonicalized absolute pathname, eliminating any symbolic links encountered in the path.

```lua
local wezterm = require 'wezterm'

-- Get the canonical path of a file
local path = wezterm.filesystem.canonicalize_path '/some/path/../file.txt'
wezterm.log_info('Canonical path: ' .. path)
```

## `dirname(path)`

This function returns the directory name of the given path.

```lua
local wezterm = require 'wezterm'

-- Get the directory name
local dir = wezterm.filesystem.dirname '/some/path/file.txt'
wezterm.log_info('Directory: ' .. dir)
```

## `basename(path)`

This function returns the base name of the given path.

```lua
local wezterm = require 'wezterm'

-- Get the base name
local base = wezterm.filesystem.basename '/some/path/file.txt'
wezterm.log_info('Base name: ' .. base)
```

## `is_absolute_path(path)`

This function checks if the given path is an absolute path.

```lua
local wezterm = require 'wezterm'

-- Check if the path is absolute
local is_absolute = wezterm.filesystem.is_absolute_path '/some/path/file.txt'
wezterm.log_info('Is absolute: ' .. tostring(is_absolute))
```

## `is_dir(path)`

This function checks if the given path is a directory.

```lua
local wezterm = require 'wezterm'

-- Check if the path is a directory
local is_directory = wezterm.filesystem.is_dir '/some/path'
wezterm.log_info('Is directory: ' .. tostring(is_directory))
```

## `is_file(path)`

This function checks if the given path is a file.

```lua
local wezterm = require 'wezterm'

-- Check if the path is a file
local is_file = wezterm.filesystem.is_file '/some/path/file.txt'
wezterm.log_info('Is file: ' .. tostring(is_file))
```

## `is_symlink(path)`

This function checks if the given path is a symbolic link.

```lua
local wezterm = require 'wezterm'

-- Check if the path is a symbolic link
local is_symlink = wezterm.filesystem.is_symlink '/some/path/link'
wezterm.log_info('Is symlink: ' .. tostring(is_symlink))
```

## `exists(path)`

This function checks if the given path exists.

```lua
local wezterm = require 'wezterm'

-- Check if the path exists
local exists = wezterm.filesystem.exists '/some/path/file.txt'
wezterm.log_info('Exists: ' .. tostring(exists))
```

## `size(path)`

This function returns the size of the file at the given path.

```lua
local wezterm = require 'wezterm'

-- Get the size of the file
local size = wezterm.filesystem.size '/some/path/file.txt'
wezterm.log_info('Size: ' .. size)
```

## `quote_path(path, method)`

This function returns a quoted version of the given path, which is useful for safely passing paths with spaces or special characters to shell commands. It is the same method used for the handling of dropped path quoting in the config. 

The `method` parameter can be either `"None"`, `"SpacesOnly"`, `"Posix"`, `"Windows"`, `"WindowsAlwaysQuoted"`, or `"SerdeJson"` to specify the quoting style. For more details about how this works please see [`quote_dropped_files` config option](../config/quote_dropped_files.md)

```lua
local wezterm = require 'wezterm'

-- Get the quoted version of the path
local quoted_path =
  wezterm.filesystem.quote_path('/some/path with spaces/file.txt', 'Posix')
```


