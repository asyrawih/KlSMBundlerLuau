local B = { name = "B" }
function B.partner()
	return require(script.Parent.A).name -- lazy: fine at runtime
end
return B
