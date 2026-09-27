-- Entry point of the resurrect plugin bundled with wezterm, used via
-- `local resurrect = require("resurrect")`.
--
-- This replaces the upstream plugin/init.lua: the upstream version locates
-- its own checkout through the third party dev.wezterm plugin (fetched from
-- GitHub at runtime) and stores the state inside that checkout. The bundled
-- version needs neither and stores the state in wezterm's data directory.

local pub = {}

local state_manager = require("resurrect.state_manager")
state_manager.change_state_save_dir(require("resurrect.native").default_state_dir())

-- Export submodules
pub.workspace_state = require("resurrect.workspace_state")
pub.window_state = require("resurrect.window_state")
pub.tab_state = require("resurrect.tab_state")
pub.fuzzy_loader = require("resurrect.fuzzy_loader")
pub.state_manager = state_manager

return pub
