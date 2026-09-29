local ReplicatedStorage = game:GetService("ReplicatedStorage")
local Greeter = require(ReplicatedStorage.Shared.Greeter)
print(Greeter.greet("server"))
