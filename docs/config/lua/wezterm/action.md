---
title: wezterm.action
tags:
 - keys
---


# `wezterm.action`


Helper for defining key assignment actions in your configuration file.
This is really just sugar for the underlying Lua -> Rust deserialization
mapping that makes it a bit easier to identify where syntax errors may
exist in your configuration file.


## Constructor Syntax


{{since('20220624-141144-bd1b7c5d')}}


`wezterm.action` is a special enum constructor type that makes it bit
more ergonomic to express the various actions than in earlier releases.
The older syntax is still supported, so you needn't scramble to update
your configuration files.


Indexing `wezterm.action` with a valid
[KeyAssignment](../keyassignment/index.md) name will act as a constructor for
