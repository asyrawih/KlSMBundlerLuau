local signal = { Connect = function(_, callback) return callback end }
signal:Connect(function()
	return require(script.Parent.B) -- only when the signal fires
end)
return { name = "A" }
