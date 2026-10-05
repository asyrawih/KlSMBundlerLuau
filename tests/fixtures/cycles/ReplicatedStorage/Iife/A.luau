local B = (function()
	return require(script.Parent.B) -- the IIFE runs right away
end)()
return { B = B }
