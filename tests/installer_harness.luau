-- Minimal Instance mock for src/installer.luau (run with the Luau CLI).
local Instance = {}
Instance.__index = function(self, key)
	if key == "Parent" then
		return rawget(self, "_parent")
	end
	return Instance[key] or self:FindFirstChild(key)
end
local function new(class, name, parent, attrs)
	local self = setmetatable({ ClassName = class, Name = name, _children = {}, _attrs = attrs or {} }, Instance)
	if parent then self:_setParent(parent) end
	return self
end
function Instance:_setParent(parent)
	if rawget(self, "_parent") then
		local list = self._parent._children
		table.remove(list, table.find(list, self))
	end
	rawset(self, "_parent", parent)
	if parent then table.insert(parent._children, self) end
end
function Instance:GetChildren() return table.clone(self._children) end
function Instance:FindFirstChild(name)
	for _, c in self._children do if c.Name == name then return c end end
	return nil
end
function Instance:GetAttribute(name) return self._attrs[name] end
function Instance:Destroy() self:_setParent(nil) end
local mt = getmetatable(new("Folder", "x"))
mt.__newindex = function(self, k, v)
	if k == "Parent" then self:_setParent(v) else rawset(self, k, v) end
end

-- The place before install: an old install with a feature since switched off, plus Studio-only
-- content the package knows nothing about.
local RS = new("ReplicatedStorage", "ReplicatedStorage")
local oldAddon = new("Folder", "Addon", RS)
local oldFeatures = new("Folder", "Features", oldAddon)
local oldAlpha = new("Folder", "Alpha", oldFeatures)
new("ModuleScript", "AlphaConfig", oldAlpha).tag = "old"
new("Folder", "Assets", oldAlpha) -- Studio-only, inside a kept feature
new("ModuleScript", "GoneConfig", new("Folder", "Gone", oldFeatures))
new("ModuleScript", "AddonLoader", RS).tag = "old"
new("Folder", "EffectDonation", RS) -- Studio-only

-- The package as dropped into ServerScriptService.
local package = new("Folder", "KlsmPackage")
local pkgRS = new("Folder", "ReplicatedStorage", package)
new("ModuleScript", "AddonLoader", pkgRS).tag = "new"
new("Script", "Loader", pkgRS).tag = "client"
local features = new("Folder", "Features", new("Folder", "Addon", pkgRS), { KlsmExact = true })
new("ModuleScript", "AlphaConfig", new("Folder", "Alpha", features)).tag = "new"
local script = new("Script", "Loader", package)
local game = { FindService = function(_, name) return if name == "ReplicatedStorage" then RS else nil end }

local function names(inst)
	local out = {}
	for _, c in inst:GetChildren() do table.insert(out, c.Name) end
	table.sort(out)
	return table.concat(out, ",")
end

--@@INSTALLER@@

local alpha = RS.Addon.Features.Alpha
print(names(RS), names(RS.Addon.Features), names(alpha), alpha.AlphaConfig.tag, RS.AddonLoader.tag, RS.Loader.tag, names(package))
