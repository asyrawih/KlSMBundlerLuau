local A = require(script.Parent.A) -- eager both ways: Roblox fails with "required recursively"
return { name = "B", partner = A }
