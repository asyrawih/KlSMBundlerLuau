local Enum = game and Enum or require "../test/mock".Enum :: never
local Other = (game and 1) or require(script.Missing)
return { Enum = Enum, Other = Other }
