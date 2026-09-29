local Counter = require(script.Parent.Util.Counter)
local Util = script.Parent.Util -- alias used only for a require below
local Again = require(Util.Counter)

local Greeter = {}
Greeter.name = script.Name
Greeter.parentName = script.Parent.Name
Greeter.sameModule = Counter == Again

function Greeter.greet(who: string): string
	local c = Counter.new()
	Counter.bump(c)
	return `hello {who} #{c.value}`
end

return Greeter
