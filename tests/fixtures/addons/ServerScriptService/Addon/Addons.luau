local ReplicatedStorage = game:GetService("ReplicatedStorage")

local AddonLoader = require(ReplicatedStorage.AddonLoader)

return AddonLoader.load(script.Parent:FindFirstChild("Features"), "Service")
