local names = {}
for _, addon in require(script.Parent.Parent.Addon.Addons) do
	table.insert(names, addon.name)
end
local alpha = require(script.Parent.Parent.Addon.Addons)[1]
local beta = require(script.Parent.Parent.Addon.Addons)[2]
print(table.concat(names, ","), alpha.helper, alpha.missing, alpha.isModule, alpha.parentIsFolder, alpha.siblings, beta.commands, beta.server)
