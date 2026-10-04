-- Boot modules load from the entry itself, through a `script` path that only makes sense at
-- the entry's original place (the bundle Script lives elsewhere).
local boots = {}
for _, boot in require(game:GetService("ReplicatedStorage").AddonLoader).load(script.Parent.Parent.Addon:FindFirstChild("Features"), "Boot") do
	table.insert(boots, boot.name)
end
local names = {}
for _, addon in require(script.Parent.Parent.Addon.Addons) do
	table.insert(names, addon.name)
end
local alpha = require(script.Parent.Parent.Addon.Addons)[1]
local beta = require(script.Parent.Parent.Addon.Addons)[2]
print(table.concat(names, ","), alpha.helper, alpha.missing, alpha.isModule, alpha.parentIsFolder, alpha.siblings, beta.commands, beta.server, table.concat(boots, ","))
