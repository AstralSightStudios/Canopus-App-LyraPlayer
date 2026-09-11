-- Run from any directory; optional second argument is a matching Lua C fixture.
local root = (arg[0]:match("^(.*[/])") or "./") .. ".."
local canopus = os.getenv("CANOPUS_ROOT") or root .. "/../Canopus"
local main = arg[1] or root .. "/watchfaces/lyra-player-prod/xiaomi-band-11/main.lua"
arg = {main, "lyra-player", arg[2]}
dofile(canopus .. "/scripts/lua/test_module_installer_prod.lua")
