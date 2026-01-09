log("lua demo loaded")

local spawned = false

function on_tick(dt)
  if not spawned then
    spawn_entity(0.0, 0.0, 0.0, 0.0)
    spawned = true
  end
end

function on_key(key, pressed)
  if pressed and key == "K" then
    play_sound("sound/misc/menu1.wav")
  end
end

function on_spawn(id, x, y, z, yaw)
  log(string.format("spawned %d at %.2f %.2f %.2f yaw %.2f", id, x, y, z, yaw))
end

register_command("spawn", function(args)
  local x = tonumber(args[1]) or 0
  local y = tonumber(args[2]) or 0
  local z = tonumber(args[3]) or 0
  local yaw = tonumber(args[4]) or 0
  spawn_entity(x, y, z, yaw)
end)

register_command("sound", function(args)
  local asset = args[1] or "sound/misc/menu1.wav"
  play_sound(asset)
end)
