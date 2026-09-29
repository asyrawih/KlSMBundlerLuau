--!strict
export type Counter = { value: number }

local Counter = {}

function Counter.new(): Counter
	return { value = 0 }
end

function Counter.bump(c: Counter): number
	c.value += 1
	return c.value
end

return Counter
