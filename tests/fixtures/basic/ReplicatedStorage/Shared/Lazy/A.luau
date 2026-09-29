local B = require(script.Parent.B)
return {
	name = "A",
	partner = function()
		return B.name
	end,
}
