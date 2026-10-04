-- KlSM package installer: before anything else runs, each service-named folder next to this
-- script is merged into that service. Scripts and modules replace same-named ones; folders
-- merge; a folder marked KlsmExact also loses children the package doesn't have (switched-off
-- features).
do
	local function install(from, into)
		local keep = {}
		for _, child in from:GetChildren() do
			keep[child.Name] = true
			local existing = into:FindFirstChild(child.Name)
			if existing and child.ClassName == "Folder" and existing.ClassName == "Folder" then
				install(child, existing)
			else
				if existing then
					existing:Destroy()
				end
				child.Parent = into
			end
		end
		if from:GetAttribute("KlsmExact") then
			for _, child in into:GetChildren() do
				if not keep[child.Name] then
					child:Destroy()
				end
			end
		end
	end
	-- Only from inside the package: a Loader moved out by hand must not touch its new parent.
	if script.Parent.ClassName == "Folder" and script.Parent.Name == "KlsmPackage" then
		for _, folder in script.Parent:GetChildren() do
			local service = folder.ClassName == "Folder" and game:FindService(folder.Name)
			if service then
				install(folder, service)
				folder:Destroy()
			end
		end
	end
end
