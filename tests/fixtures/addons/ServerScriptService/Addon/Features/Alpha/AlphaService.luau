return {
	name = "Alpha",
	helper = require(script.Parent.AlphaHelper),
	missing = script.Parent:FindFirstChild("Nope") == nil,
	isModule = script:IsA("ModuleScript") and script.ClassName == "ModuleScript",
	parentIsFolder = script.Parent.ClassName == "Folder" and not script.Parent:IsA("ModuleScript"),
	siblings = #script.Parent:GetChildren(),
}
