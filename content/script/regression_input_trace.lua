log("regression input trace loaded")

local TRACE_NAME = "m9_regression"
local START_DELAY_SECONDS = 0.5
local RECORD_SECONDS = 6.0

local elapsed = 0.0
local state = "waiting"

local function start_record(name)
  cmd("dev_collision_draw 1")
  cmd("dev_input_record " .. name)
  log("regression input trace recording started")
end

local function stop_record()
  cmd("dev_input_record_stop")
  log("regression input trace recording stopped")
end

local function start_replay(name)
  cmd("dev_collision_draw 1")
  cmd("dev_input_replay " .. name)
  log("regression input trace replay started")
end

function on_tick(dt)
  elapsed = elapsed + dt
  if state == "waiting" then
    if elapsed < START_DELAY_SECONDS then
      return
    end
    elapsed = 0.0
    state = "recording"
    start_record(TRACE_NAME)
    return
  end
  if state == "recording" then
    if elapsed < RECORD_SECONDS then
      return
    end
    stop_record()
    elapsed = 0.0
    state = "replay"
    start_replay(TRACE_NAME)
    return
  end
end

register_command("record_regression", function(args)
  local name = args[1] or TRACE_NAME
  local seconds = tonumber(args[2]) or RECORD_SECONDS
  elapsed = 0.0
  TRACE_NAME = name
  RECORD_SECONDS = seconds
  state = "recording"
  start_record(name)
end)

register_command("run_regression", function(args)
  local name = args[1] or TRACE_NAME
  start_replay(name)
end)
