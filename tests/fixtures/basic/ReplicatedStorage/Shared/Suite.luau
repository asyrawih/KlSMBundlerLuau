-- ModuleScript entry used by the runtime test: returns everything it observed.
local ReplicatedStorage = game:GetService("ReplicatedStorage")
local Shared = ReplicatedStorage:WaitForChild("Shared")

local Greeter = require(Shared.Greeter)
local Helper = require("./Helper")
local Pkg = require(
	ReplicatedStorage.Pkg
)
local A = require(Shared.Lazy.A)
local B = require(Shared["Lazy"]:FindFirstChild("B"))
local Twice1 = require(script.Parent.Twice)
local Twice2 = require("@self/../Twice")

type G = typeof(require(Shared.Greeter))

local brokenOk, brokenErr = pcall(function()
	return require(Shared.Broken)
end)

return {
	greet = Greeter.greet("roblox"),
	name = Greeter.name,
	parentName = Greeter.parentName,
	sameModule = Greeter.sameModule,
	helper = Helper(21),
	pkg = Pkg.version,
	aPartner = A.partner(),
	bPartner = B.partner(),
	cached = Twice1 == Twice2 and Twice1.loads == 1,
	brokenOk = brokenOk,
	brokenErr = tostring(brokenErr),
}
