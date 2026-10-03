-- Same shape as RessoMusic's loader: scans a folder at runtime and requires each match.
local function load(features, suffix)
	local modules = {}
	if not features then
		return modules
	end
	local folders = features:GetChildren()
	table.sort(folders, function(a, b)
		return a.Name < b.Name
	end)
	for _, folder in folders do
		local module = folder:FindFirstChild(folder.Name .. suffix)
		if not module or not module:IsA("ModuleScript") then
			continue
		end
		local ok, result = pcall(require, module)
		if ok then
			table.insert(modules, result)
		else
			warn(`skipped {folder.Name}{suffix}`)
		end
	end
	return modules
end

return { load = load }
