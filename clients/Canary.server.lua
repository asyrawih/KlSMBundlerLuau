-- Canary: detak ke Discord. Berhenti tanpa "⚪ close" = server crash.
-- Taruh di ServerScriptService sebagai Script.
-- Syarat: Game Settings → Security → Allow HTTP Requests = ON
local HttpService = game:GetService("HttpService")
local Stats = game:GetService("Stats")
local Players = game:GetService("Players")
local RunService = game:GetService("RunService")

local WEBHOOK = "https://discord.com/api/webhooks/GANTI_INI" -- channel privat
local INTERVAL = 5
local RX_ALERT = 250  -- baseline normal ≤90 kbps; naikkan ke 300 kalau ada alert palsu di jam ramai
local HB_ALERT = 0.1  -- detik; heartbeat > 100ms = server tersendat

if RunService:IsStudio() then return end

local function post(msg)
	local ok, err = pcall(HttpService.PostAsync, HttpService, WEBHOOK,
		HttpService:JSONEncode({ content = msg }), Enum.HttpContentType.ApplicationJson)
	if not ok then warn("[Canary] post gagal:", err) end
end

local function line()
	local rx, phys, hb = Stats.DataReceiveKbps, Stats.PhysicsReceiveKbps, Stats.HeartbeatTime
	local text = ("`%s` players=%d rx=%.0f phys=%.0f hb=%.0fms")
		:format(game.JobId:sub(1, 8), #Players:GetPlayers(), rx, phys, hb * 1000)
	local bad = rx > RX_ALERT or hb > HB_ALERT
	return (bad and "🔴 " or "") .. text, bad
end

task.spawn(function()
	post(("🟢 start `%s` private=%s"):format(game.JobId, tostring(game.PrivateServerId ~= "")))
	local quiet = 0
	while task.wait(INTERVAL) do
		local text, bad = line()
		quiet += INTERVAL
		if bad or quiet >= 60 then -- 🔴 langsung, normal tiap 60 detik
			task.spawn(post, text)
			quiet = 0
		end
	end
end)

game:BindToClose(function()
	post(("⚪ close `%s` (shutdown normal, bukan crash)"):format(game.JobId:sub(1, 8)))
end)
