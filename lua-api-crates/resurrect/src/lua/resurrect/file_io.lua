local wezterm = require("wezterm") --[[@as Wezterm]] --- this type cast invokes the LSP module for Wezterm

local pub = {
	encryption = { enable = false },
}

-- Write a file with the content of a string
---@param file_path string full filename
---@return boolean success result
---@return string|nil error
function pub.write_file(file_path, str)
	local suc, err = pcall(function()
		local handle = io.open(file_path, "w+")
		if not handle then
			error("Could not open file: " .. file_path)
		end
		handle:write(str)
		handle:flush()
		handle:close()
	end)
	return suc, err
end

-- Read a file and return its content
---@param file_path string full filename
---@return boolean success result
---@return string|nil error
function pub.read_file(file_path)
	local stdout
	local suc, err = pcall(function()
		local handle = io.open(file_path, "r")
		if not handle then
			error("Could not open file: " .. file_path)
		end
		stdout = handle:read("*a")
		handle:close()
	end)
	if suc then
		return suc, stdout
	else
		return suc, err
	end
end

--- Merges user-supplied options with default options
--- @param user_opts encryption_opts
function pub.set_encryption(user_opts)
	pub.encryption = require("resurrect.encryption")
	for k, v in pairs(user_opts) do
		if v ~= nil then
			pub.encryption[k] = v
		end
	end
end

--- Sanitize the input by replacing control characters and invalid UTF-8 sequences with valid \uxxxx unicode
--- @param data string
--- @return string
local function sanitize_json(data)
	wezterm.emit("resurrect.file_io.sanitize_json.start", data)
	-- escapes control characters to ensure valid json
	data = data:gsub("[\x00-\x1F]", function(c)
		return string.format("\\u00%02X", string.byte(c))
	end)
	wezterm.emit("resurrect.file_io.sanitize_json.finished")
	return data
end

---@param file_path string
---@param state table
---@param event_type "workspace" | "window" | "tab"
function pub.write_state(file_path, state, event_type)
	wezterm.emit("resurrect.file_io.write_state.start", file_path, event_type)
	local json_state = wezterm.json_encode(state)
	json_state = sanitize_json(json_state)
	if pub.encryption.enable then
		wezterm.emit("resurrect.file_io.encrypt.start", file_path)
		local ok, err = pcall(function()
			return pub.encryption.encrypt(file_path, json_state)
		end)
		if not ok then
			wezterm.emit("resurrect.error", "Encryption failed: " .. tostring(err))
			wezterm.log_error("Decryption failed: " .. tostring(err))
		else
			wezterm.emit("resurrect.file_io.encrypt.finished", file_path)
		end
	else
		local ok, err = pub.write_file(file_path, json_state)
		if not ok then
			wezterm.emit("resurrect.error", "Failed to write state: " .. err)
			wezterm.log_error("Failed to write state: " .. err)
		end
	end
	wezterm.emit("resurrect.file_io.write_state.finished", file_path, event_type)
end

---@param file_path string
---@return table|nil
function pub.load_json(file_path)
	local json
	if pub.encryption.enable then
		wezterm.emit("resurrect.file_io.decrypt.start", file_path)
		local ok, output = pcall(function()
			return pub.encryption.decrypt(file_path)
		end)
		if not ok then
			wezterm.emit("resurrect.error", "Decryption failed: " .. tostring(output))
			wezterm.log_error("Decryption failed: " .. tostring(output))
		else
			json = output
			wezterm.emit("resurrect.file_io.decrypt.finished", file_path)
		end
	else
		local lines = {}
		for line in io.lines(file_path) do
			table.insert(lines, line)
		end
		json = table.concat(lines)
	end
	if not json then
		return nil
	end
	json = sanitize_json(json)

	return wezterm.json_parse(json)
end

return pub
