local wezterm = require("wezterm") --[[@as Wezterm]] --- this type cast invokes the LSP module for Wezterm
local utils = require("resurrect.utils")
local file_io = require("resurrect.file_io")
local pub = {}

---@alias fmt_fun fun(label: string): string
---@alias fuzzy_load_opts {title: string, description: string, fuzzy_description: string, is_fuzzy: boolean,
---ignore_workspaces: boolean, ignore_tabs: boolean, ignore_windows: boolean, fmt_window: fmt_fun, fmt_workspace: fmt_fun,
---fmt_tab: fmt_fun, fmt_date: fmt_fun, show_state_with_date: boolean, date_format: string, ignore_screen_width: boolean,
---name_truncature: string, min_filename_size: number}

---Default fuzzy loading options
---@type fuzzy_load_opts
pub.default_fuzzy_load_opts = {
	title = "Load State",
	description = "Select State to Load and press Enter = accept, Esc = cancel, / = filter",
	fuzzy_description = "Search State to Load: ",
	is_fuzzy = true,
	ignore_workspaces = false,
	ignore_windows = false,
	ignore_tabs = false,
	ignore_screen_width = true,
	date_format = "%d-%m-%Y %H:%M:%S",
	show_state_with_date = false,
	name_truncature = " " .. wezterm.nerdfonts.cod_ellipsis .. "  ",
	min_filename_size = 10,
	fmt_date = function(date)
		return wezterm.format({
			{ Foreground = { AnsiColor = "White" } },
			{ Text = date },
		})
	end,
	fmt_workspace = function(label)
		return wezterm.format({
			{ Foreground = { AnsiColor = "Green" } },
			{ Text = "󱂬 : " .. label:gsub("(.*)%.json(.*)", "%1%2") },
		})
	end,
	fmt_window = function(label)
		return wezterm.format({
			{ Foreground = { AnsiColor = "Yellow" } },
			{ Text = " : " .. label:gsub("(.*)%.json(.*)", "%1%2") },
		})
	end,
	fmt_tab = function(label)
		return wezterm.format({
			{ Foreground = { AnsiColor = "Red" } },
			{ Text = "󰓩 : " .. label:gsub("(.*)%.json(.*)", "%1%2") },
		})
	end,
}

-- Recursive JSON file finder.
-- This is implemented natively by wezterm rather than by spawning helper
-- processes (VBS scripts via wscript.exe on Windows, find/stat/awk elsewhere).
-- The output has one "<mtime epoch seconds> <path>" line per file.
---@param base_path string starting path from which the recursive search takes place
---@return string|nil
local function find_json_files_recursive(base_path)
	local suc, stdout = pcall(require("resurrect.native").list_json_files, base_path)
	if suc then
		return stdout
	else
		wezterm.emit("resurrect.error", tostring(stdout))
		return
	end
end

-- build a table with the output of the file finder function
---@param stdout string|nil
---@param opts table
---@return table
local function insert_choices(stdout, opts)
	-- this structure will contain the formatting costs for each elements
	local fmt_cost = {}
	-- pre-calculation of formatting cost
	local types = { "workspace", "window", "tab" }
	local state_files = {}
	local files = {
		workspace = {},
		window = {},
		tab = {},
	}
	local max_length = 0

	if stdout == nil then
		return state_files
	end

	-- Parse the stdout and construct the file table
	for line in stdout:gmatch("[^\n]+") do
		local epoch, type, file = line:match("%s*(%d+)%s+.+[/\\]([^/\\]+)[/\\]([^/\\]+%.json)$")
		-- epoch in this case represents the last modified date/time according to the OS
		-- For Unix/POSIX Epoch is counted from January 1st, 1970 0 UTC
		-- MacOS it is from January 1st, 1904 0 UTC
		-- Windows NTFS (up to Win 11) it is from January 1st, 1601 0 UTC
		-- The function `os.date()` used later on will convert the date according to the host OS
		if epoch and file and type and not opts[string.format("ignore_%ss", type)] then
			-- consider the "cost" of the formatting of the filename, i.e., if the format function adds characters
			-- to the visible part of the file section, we test the three possible formatter to get the highest cost
			-- we use a real entry instead of an empty string to prevent formatting error if the format function has
			-- expectations to work correctly
			-- This prevent from having to format every filename, instead we can take the filename length and then
			-- the cost of formatting per type
			--
			if next(fmt_cost) == nil then
				fmt_cost.workspace = 0 -- cost of formatting the workspace name
				fmt_cost.window = 0 -- cost of formatting the window name
				fmt_cost.tab = 0 -- cost of formatting the tab
				fmt_cost.str_date = 0 -- cost of date as a string
				fmt_cost.fmt_date = 0 -- cost of formatting the date
				-- Calculate the cost for formatting the filename
				local len = utils.utf8len(file)
				for _, t in ipairs(types) do
					if not opts[string.format("ignore_%ss", t)] then
						local fmt = opts[string.format("fmt_%s", t)]
						if fmt then
							fmt_cost[t] = utils.utf8len(utils.strip_format_esc_seq(fmt(file))) - len
						end
					end
				end
				-- Calculate the cost for formatting the date
				if opts.show_state_with_date then
					local str_date = " " .. os.date(opts.date_format, tonumber(epoch))
					fmt_cost.str_date = utils.utf8len(str_date)
					if opts.fmt_date then
						fmt_cost.fmt_date = utils.utf8len(utils.strip_format_esc_seq(opts.fmt_date(str_date)))
							- fmt_cost.str_date
					end
				end
			end

			-- Calculating the maximum file length
			local filename_len = utils.utf8len(file) + fmt_cost[type] -- we keep this so we don't have to measure it later
			max_length = math.max(max_length, filename_len)

			local date = ""
			if opts.show_state_with_date then
				date = " " .. os.date(opts.date_format, tonumber(epoch))
				if opts.fmt_date then
					date = opts.fmt_date(date)
				end
			end
			local date_len = utils.utf8len(utils.strip_format_esc_seq(date))

			-- collecting all relevant information about the file
			local fmt = opts[string.format("fmt_%s", type)]
			table.insert(files[type], {
				id = type .. utils.separator .. file,
				filename = file,
				filename_len = filename_len,
				date = date,
				date_len = date_len,
				fmt = fmt,
			})
		end
	end

	local available_width
	if opts.ignore_screen_width then
		available_width = max_length + fmt_cost.str_date + fmt_cost.fmt_date
	else
		-- During the selection view, InputSelector will take 4 characters on the left and 2 characters
		-- on the right of the window
		available_width = utils.get_current_window_width() - 6
	end

	-- constants used to shorten the file name if necessary
	local str_pad = opts.name_truncature or "..."
	local pad_len = utils.utf8len(str_pad)
	local min_filename_len = opts.min_filename_size or 10 -- minimum size of the filename to remain decypherable

	-- Add files to state_files list and apply the formatting functions
	for _, type in ipairs(types) do
		for _, file in ipairs(files[type]) do
			local label = file.filename
			local dots = ""

			local filename_date_len = file.filename_len + file.date_len

			-- we prepare here the dots separator between the file name and the date, taking into account the space available
			-- if not enough space available the separator will be limited to the single space that is prefixing the date
			-- if there is enough space, we can make the display prettier by have a space between the file name and the
			-- dots separator
			if opts.show_state_with_date then
				local dots_len = math.max(available_width - filename_date_len, 0)
				dots = string.rep(".", dots_len)

				-- if there is enough room we can have a space between the filename and the dots
				if #dots > 3 then
					dots = " " .. dots:sub(2)
				end
			end

			-- to fit in the space we use we would need to reduce the filename by that much
			-- but keeping in mind that we don't want the name to become too small
			if filename_date_len + #dots > available_width then
				-- Formulas kept for documentation:
				-- 1. calculate the necessary reduction of the filename
				-- local reduction = file.filename_len + file.date_len + pad_len + #dots - available_width
				-- 2. correction of the reduction in case the resulting name length is smaller than the minimym
				-- reduction = file.filename_len - math.max(file.filename_len - reduction, min_filename_len + pad_len)
				-- 3. putting things together in a single formula
				local reduction = file.filename_len
					- math.max(available_width - file.date_len - pad_len - #dots, min_filename_len + pad_len)
				label = utils.replace_center(label, reduction, str_pad)
			end

			-- and now everything comes together
			label = label .. dots
			if file.fmt then
				label = file.fmt(label)
			end
			label = label .. file.date

			table.insert(state_files, { id = file.id, label = label })
		end
	end
	return state_files
end

---A fuzzy finder to restore saved state
---@param window MuxWindow
---@param pane Pane
---@param callback fun(id: string, label: string, save_state_dir: string)
---@param opts fuzzy_load_opts?
function pub.fuzzy_load(window, pane, callback, opts)
	wezterm.emit("resurrect.fuzzy_loader.fuzzy_load.start", window, pane)

	opts = utils.tbl_deep_extend("force", pub.default_fuzzy_load_opts, opts or {})

	local folder = require("resurrect.state_manager").save_state_dir

	-- Always use the recursive search function
	local stdout = find_json_files_recursive(folder)

	-- build the choice list for the InputSelector
	local state_files = insert_choices(stdout, opts)

	if #state_files == 0 then
		wezterm.emit("resurrect.error", "No existing state files to select")
	end

	-- even if the list is empty, user experience is better if we show an empty list
	window:perform_action(
		wezterm.action.InputSelector({
			action = wezterm.action_callback(function(_, _, id, label)
				if id and label then
					callback(id, label, require("resurrect.state_manager").save_state_dir)
				end
				wezterm.emit("resurrect.fuzzy_loader.fuzzy_load.finished", window, pane)
			end),
			title = opts.title,
			description = opts.description,
			fuzzy_description = opts.fuzzy_description,
			choices = state_files,
			fuzzy = opts.is_fuzzy,
		}),
		pane
	)
end

return pub
