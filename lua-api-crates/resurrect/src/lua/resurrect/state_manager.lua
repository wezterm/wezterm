local wezterm = require("wezterm") --[[@as Wezterm]] --- this type cast invokes the LSP module for Wezterm
local file_io = require("resurrect.file_io")
local utils = require("resurrect.utils")

local pub = {}

---@param file_name string
---@param type string
---@param opt_name string?
---@return string
local function get_file_path(file_name, type, opt_name)
	if opt_name then
		file_name = opt_name
	end
	return string.format(
		"%s%s" .. utils.separator .. "%s.json",
		pub.save_state_dir,
		type,
		file_name:gsub(utils.separator, "+")
	)
end

---save state to a file
---@param state workspace_state | window_state | tab_state
---@param opt_name? string
function pub.save_state(state, opt_name)
	if state.window_states then
		file_io.write_state(get_file_path(state.workspace, "workspace", opt_name), state, "workspace")
	elseif state.tabs then
		file_io.write_state(get_file_path(state.title, "window", opt_name), state, "window")
	elseif state.pane_tree then
		file_io.write_state(get_file_path(state.title, "tab", opt_name), state, "tab")
	end
end

---Reads a file with the state
---@param name string
---@param type string
---@return table
function pub.load_state(name, type)
	wezterm.emit("resurrect.state_manager.load_state.start", name, type)
	local json = file_io.load_json(get_file_path(name, type))
	if not json then
		wezterm.emit("resurrect.error", "Invalid json: " .. get_file_path(name, type))
		return {}
	end
	wezterm.emit("resurrect.state_manager.load_state.finished", name, type)
	return json
end

---Saves the stater after interval in seconds
---@param opts? { interval_seconds: integer?, save_workspaces: boolean?, save_windows: boolean?, save_tabs: boolean? }
function pub.periodic_save(opts)
	if opts == nil then
		opts = { save_workspaces = true }
	end
	if opts.interval_seconds == nil then
		opts.interval_seconds = 60 * 15
	end
	wezterm.time.call_after(opts.interval_seconds, function()
		wezterm.emit("resurrect.state_manager.periodic_save.start", opts)
		if opts.save_workspaces then
			pub.save_state(require("resurrect.workspace_state").get_workspace_state())
		end

		if opts.save_windows then
			for _, gui_win in ipairs(wezterm.gui.gui_windows()) do
				local mux_win = gui_win:mux_window()
				local title = mux_win:get_title()
				if title ~= "" and title ~= nil then
					pub.save_state(require("resurrect.window_state").get_window_state(mux_win))
				end
			end
		end

		if opts.save_tabs then
			for _, gui_win in ipairs(wezterm.gui.gui_windows()) do
				local mux_win = gui_win:mux_window()
				for _, mux_tab in ipairs(mux_win:tabs()) do
					local title = mux_tab:get_title()
					if title ~= "" and title ~= nil then
						pub.save_state(require("resurrect.tab_state").get_tab_state(mux_tab))
					end
				end
			end
		end

		wezterm.emit("resurrect.state_manager.periodic_save.finished", opts)
		pub.periodic_save(opts)
	end)
end

---Writes the current state name and type
---@param name string
---@param type string
---@return boolean
---@return string|nil
function pub.write_current_state(name, type)
	local file_path = pub.save_state_dir .. utils.separator .. "current_state"
	local suc, err = file_io.write_file(file_path, string.format("%s\n%s", name, type))
	return suc, err
end

---callback for resurrecting workspaces on startup
---@return boolean
---@return string|nil
function pub.resurrect_on_gui_startup()
	local file_path = pub.save_state_dir .. utils.separator .. "current_state"
	local suc, err = pcall(function()
		local file = io.open(file_path, "r")
		if not file then
			error("Could not open file: " .. file_path)
		end
		local name = file:read("*line")
		local type = file:read("*line")
		file:close()
		if type == "workspace" then
			require("resurrect.workspace_state").restore_workspace(pub.load_state(name, type), {
				spawn_in_workspace = true,
				relative = true,
				restore_text = true,
				on_pane_restore = require("resurrect.tab_state").default_on_pane_restore,
			})
			wezterm.mux.set_active_workspace(name)
		end
	end)
	return suc, err
end

---@param file_path string
function pub.delete_state(file_path)
	wezterm.emit("resurrect.state_manager.delete_state.start", file_path)
	local path = pub.save_state_dir .. file_path
	local success = os.remove(path)
	if not success then
		wezterm.emit("resurrect.error", "Failed to delete state: " .. path)
		wezterm.log_error("Failed to delete state: " .. path)
	end
	wezterm.emit("resurrect.state_manager.delete_state.finished", file_path)
end

--- Merges user-supplied options with default options
--- @param user_opts encryption_opts
function pub.set_encryption(user_opts)
	require("resurrect.file_io").set_encryption(user_opts)
end

---Changes the directory to save the state to
---@param directory string
function pub.change_state_save_dir(directory)
	local types = { "workspace", "window", "tab" }
	for _, type in ipairs(types) do
		utils.ensure_folder_exists(directory .. "/" .. type)
	end
	pub.save_state_dir = directory
end

function pub.set_max_nlines(max_nlines)
	require("resurrect.pane_tree").max_nlines = max_nlines
end

return pub
