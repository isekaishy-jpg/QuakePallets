# Camera–Control Frame Spec v1
**Defines how camera and movement frames interact across RPG and arena modes.**

## Units and conventions
- **Distance:** meters (world units are meters)
- **Time:** seconds
- **Angles:** radians in code; degrees are allowed in authored config (e.g., TOML) and must be converted at load
- **Velocity:** m/s
- **Acceleration:** m/s²
- **Gravity:** m/s² (project default should be stated in the controller/collision layer)

Rapier is unitless; this project standardizes on the scale above for numeric stability and consistency.


## 1) Definitions

### Units note
- Angles are radians in code; any degrees in authored config must be converted during load.

- `camera_yaw`, `camera_pitch`: the view angles used to orient the render camera.
- `character_yaw`: the facing direction used by RPG movement (and optionally by arena).
- `turn_vs_strafe_mode`: whether A/D input maps to turn or strafe.
- `camera_lock_to_character`: when true, character yaw tracks camera yaw.
- `strafe_always`: when true, A/D always strafe (forces lock/strafe behavior).

## 2) RPG mode (MMO-style)
### 2.1 Default (not strafing / free-look orbit)
- Camera may orbit around the character (third-person follow).
- `character_yaw` is controlled by “turning” inputs or explicit rotation logic (gameplay dependent).
- `turn_vs_strafe_mode`:
  - if `strafe_always == false`: A/D may be turn (optional) and camera stays in orbit behavior
  - if `strafe_always == true`: A/D is strafe and camera behaves in lock state

### 2.2 RMB held (strafe/lock state)
- `camera_lock_to_character = true`
- `character_yaw` tracks `camera_yaw` (mouse movement drives turning)
- `turn_vs_strafe_mode = strafe` (A/D strafe)
- Movement motor receives:
  - `move_forward`, `move_strafe`
  - `character_yaw_rad` (now matching camera yaw while RMB held)

### 2.3 Backpedal behavior
- Only backpedal is slower (per RPG spec).
- Strafing is full speed.

## 3) Arena mode (Golden Angle)
### 3.1 v1 default
- `character_yaw == camera_yaw` always
- `turn_vs_strafe_mode = strafe` always (A/D strafe)
- `camera_lock_to_character = true` effectively (no decouple in normal play)

### 3.2 Minimal Option B hook (reserved)
Reserve a control surface for future special cases:
- allow `camera_lock_to_character = false` in rare modes (vehicles/turrets/etc.)
- do not implement a large mode system in v1; just keep the flags and APIs.

## 4) Update order (deterministic)
1) Sample raw input (mouse delta, RMB state, keys)
2) Map to `camera_yaw/pitch` updates and mode flags (`camera_lock_to_character`, `strafe_always`)
3) Resolve `character_yaw` based on mode rules above
4) Emit:
   - `player_view_control`
   - motor inputs (`move_forward`, `move_strafe`, `character_yaw_rad`)
5) Update camera rig(s) → obstruction → smoothing → final camera transforms
