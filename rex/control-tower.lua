-- control-tower's Rex actions: the dashboard's tab and its popup.
--
-- brew installs this file with its own prefix written in, and nothing loads it until you
-- do. Add to ~/.config/rex/init.lua, with the keys you want:
--
--   dofile("@HOMEBREW_PREFIX@/opt/control-tower/share/control-tower/rex/control-tower.lua")
--   rex.bind("<key>", "tower_space")
--   rex.bind("<key>", "tower_popup")
--
-- then run `rex config reload`.
--
-- tower_space shows the dashboard's tab, opening one in this session if there is none;
-- pressed in the dashboard's tab, it goes back to the tab it was last pressed in. q in the
-- dashboard only closes it. tower_popup opens the dashboard over this tab, 85% each way;
-- q, or Esc at its top level, closes it. Both only move what the app shows from your key
-- press: Remote Control is never needed.

local prefix = "@HOMEBREW_PREFIX@"
local tower = prefix .. "/bin/tower"
-- Rex runs a command with its own environment, which has no Homebrew PATH: tower finds
-- rex there (README, Install).
local env = { PATH = prefix .. "/bin:/usr/bin:/bin" }

-- `text` without the line breaks and spaces that end it.
local function trimmed(text)
  return (text:gsub("%s+$", ""))
end

-- Runs `tower <command>` for the key pressed at ctx, and returns what it printed and its
-- stderr, trimmed. A tower that fails raises its reason, which already starts "tower:",
-- and Rex shows it.
local function run(command, ctx)
  if ctx.session_id == nil or ctx.block_id == nil then
    error("control-tower: Rex gave this key no session or block to act on", 0)
  end
  local r, err = rex.exec(
    { tower, command, "--session", ctx.session_id, "--block", ctx.block_id },
    { env = env, timeout = 20 })
  if r == nil then
    error("control-tower: cannot run " .. tower .. ": " .. trimmed(err), 0)
  end
  local said = trimmed(r.stderr)
  if not r.ok then
    error(said ~= "" and said or ("tower " .. command .. " exited " .. r.code), 0)
  end
  return r.stdout, said
end

rex.action{
  name = "tower_space",
  title = "control-tower: Dashboard",
  description = "Show the control-tower dashboard's tab, or from it, go back.",
  run = function(ctx, args)
    local out, said = run("space", ctx)
    -- Why nothing moves, or what the key passed over.
    if said ~= "" then
      rex.log("info", said)
    end
    local place = rex.json.decode(out)
    if place.session_id ~= nil then
      rex.client.queue("session.select", place)
    end
  end,
}

rex.action{
  name = "tower_popup",
  title = "control-tower: Dashboard Popup",
  description = "Open the control-tower dashboard in a popup over this tab.",
  run = function(ctx, args)
    run("popup", ctx)
  end,
}
