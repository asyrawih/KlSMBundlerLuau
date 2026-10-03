-- Alias lines like these run at runtime even though they only feed requires.
local Commands = script:FindFirstAncestor("Features").Beta.Commands
local loaded = {}
for _, module in { Commands.Hat, Commands.Move } do
	table.insert(loaded, require(module))
end
local Server = script:FindFirstAncestor("Addon").Parent.Server
return { name = "Beta", commands = table.concat(loaded, "+"), server = Server.Name }
