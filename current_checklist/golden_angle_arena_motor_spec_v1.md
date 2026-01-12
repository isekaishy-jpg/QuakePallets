# Golden Angle — Arena Motor Spec v1 (Rapier KCC)

## Units and conventions
- **Distance:** meters (world units are meters)
- **Time:** seconds
- **Angles:** radians in code; degrees are allowed in authored config (e.g., TOML) and must be converted at load
- **Velocity:** m/s
- **Acceleration:** m/s²
- **Gravity:** m/s² (project default should be stated in the controller/collision layer)

Rapier is unitless; this project standardizes on the scale above for numeric stability and consistency.


## Purpose
Define the **Arena Motor** portion of the Golden Angle movement system: how player input becomes a **desired planar velocity** (and jump intent) at a fixed simulation tick.

This document intentionally avoids legacy/third‑party naming. Collision resolution, stepping, and grounded detection are delegated to **Rapier’s Kinematic Character Controller (KCC)** via `character_collision`.

---

## 1) Concepts and invariants

### 1.1 Simulation model
- The motor runs at a **fixed tick**.
- The motor does **not** perform collision queries, stepping, plane clipping, or depenetration.
- The motor produces:
  - desired planar velocity update (and optional vertical velocity contribution if you represent jump that way)
  - jump intent (edge / held / buffered policy)
  - optional debug metrics (speed, angle delta, gain factors)

### 1.2 Inputs
At each tick the motor receives:
- `move_forward` in [-1, +1]
- `move_strafe`  in [-1, +1]
- `jump_pressed` (edge)
- `jump_held` (level) (optional; can be derived)
- `view_yaw_rad` (camera/controller yaw)

### 1.3 Collision/KCC state provided to the motor
At each tick the motor consumes KCC-derived state (read-only):
- `is_grounded: bool`
- `ground_normal: vec3` (valid when grounded)
- `dt: seconds`

### 1.4 Outputs
- Updated `vel` (at minimum planar XZ / XY depending on your axis convention)
- `request_jump: bool` (true when the motor wants a jump to occur this tick)
- Optional: `movement_flags` (e.g., “friction_suppressed_this_tick”)

---

## 2) Terminology: Move Intent
Instead of “wish direction/speed,” Golden Angle uses **Move Intent**:

- `move_intent_dir`: unit vector (world-space, planar or ground-tangent)
- `move_intent_mag`: scalar in [0, 1] (input magnitude)
- `move_intent_speed`: `move_intent_mag * max_speed` (mode-dependent cap)

**Why split dir and magnitude**
- `move_intent_dir` is stable for angle-based gain and corridor shaping.
- `move_intent_mag` cleanly supports partial input (even if you only ship keyboard today).

---

## 3) Building Move Intent

### 3.1 Raw planar intent (yaw-relative)
Let:
- `forward2 = (cos(yaw), sin(yaw))`
- `right2   = (-sin(yaw), cos(yaw))`

Compute:
- `intent2 = forward2 * move_forward + right2 * move_strafe`

Then:
- `move_intent_mag = clamp(length(intent2), 0, 1)`
- `move_intent_dir_planar = normalize(intent2)` if mag > 0 else (0,0)

### 3.2 Ground-tangent projection (recommended)
When grounded, project the planar direction onto the **ground tangent plane**:

- `dir3 = (move_intent_dir_planar.x, 0, move_intent_dir_planar.y)` (or your axis convention)
- `dir3_tangent = normalize(dir3 - ground_normal * dot(dir3, ground_normal))` if non-zero

Use `dir3_tangent` as `move_intent_dir` for grounded movement.
In air, use the planar direction directly.

This makes slope traversal consistent without introducing steering assistance.

---

## 4) Speed caps
Arena motor uses two caps (tunable):
- `max_speed_ground`
- `max_speed_air` (often equal or slightly lower)

Define:
- `move_intent_speed = move_intent_mag * (is_grounded ? max_speed_ground : max_speed_air)`

---

## 5) Core motor primitives

### 5.1 Planar acceleration toward intent
Update planar velocity toward intent speed:

Inputs:
- `vel_planar`
- `move_intent_dir` (unit)
- `move_intent_speed`
- `accel` (ground or air)
- `dt`

Rule (projection-limited accelerate):
- `current_along = dot(vel_planar, move_intent_dir)`
- `add = move_intent_speed - current_along`
- if `add <= 0`: no-op
- `push = min(accel * dt * move_intent_speed, add)`
- `vel_planar += move_intent_dir * push`

This form makes acceleration scale sensibly with desired speed cap.

### 5.2 Ground friction with stop floor (for crisp stop)
When grounded, apply friction unless suppressed (see §6 frictionless jump):

- `speed = length(vel_planar)`
- if `speed < eps`: return
- `drop = max(speed, stop_speed) * friction * dt`
- `new_speed = max(speed - drop, 0)`
- `vel_planar *= new_speed / speed`

Tunables:
- `friction`
- `stop_speed` (floor that gives crisp stopping)

### 5.3 Corridor shaping (modest air steering assist)
A constrained “rotate toward intent” term used primarily in air (and optionally very lightly on ground):

- Only apply when `speed > min_speed_for_shaping`
- Rotate `vel_planar` toward `move_intent_dir` by at most `shaping_strength * dt` radians
- Constrain by `max_shaping_angle_per_tick` and/or require `dot(vel_dir, move_intent_dir) >= min_alignment`

Goal: help keep lines through doorways at speed without overriding player skill.

Tunables:
- `shaping_strength`
- `min_speed_for_shaping`
- `max_shaping_angle_per_tick`
- `min_alignment`

---

## 6) Frictionless Jump (soft) + Jump Buffering (required)

### 6.1 Jump buffering (required)
Policy:
- If jump is pressed while jumping is not currently possible, start a buffer timer.
- If the player becomes grounded while the buffer is active, trigger a jump immediately.

Tunables:
- `jump_buffer_window_ticks` (or seconds)

### 6.2 Frictionless jump mode (required knob)
Expose:
- `frictionless_jump_mode = none | soft | hard` (arena default: **soft**)

### 6.3 Soft frictionless jump (arena default)
Soft mode is defined strictly as a **friction application rule**, not as collision behavior.

Recommended rule (simple, robust):
- If `request_jump` is true this tick, **skip ground friction this tick**.
- Additionally, if grounded and (`jump_held` or buffered-jump pending), allow a **small grace window** where friction is reduced:
  - `landing_grace_ticks`
  - `landing_friction_scale` in [0, 1]

This preserves speed across chained jumps without requiring exotic air accel.

Tunables:
- `landing_grace_ticks`
- `landing_friction_scale`

### 6.4 Hard frictionless jump (future)
Hard mode may further suppress friction while actively chaining jumps, but should be explicitly constrained (anti-degenerate rules) and is out of scope for v1.

---

## 7) Golden Angle gain (arena signature mechanic)
Golden Angle introduces a speed-dependent gain that rewards a target offset angle between:
- `vel_dir` (current planar velocity direction)
- `view_forward_dir` (camera yaw forward direction)

### 7.1 Angle delta
- `theta = angle_between(vel_dir, view_forward_dir)` in radians (or degrees)

### 7.2 Gain curve
Define a normalized quality ramp from straight-ahead to the target angle:

- `q = saturate(theta / theta_target)` (0 at straight-ahead, 1 at or beyond target)
- Optional smoothing: `q = q*q*(3 - 2*q)` (smoothstep)
- `g(theta) = lerp(g_min, g_peak, q)`

Then blend in by speed:
- `t = saturate((speed - blend_speed_start) / (blend_speed_end - blend_speed_start))`
- `gain = lerp(1.0, g(theta), t)`

Apply gain to the **air acceleration term** and the airborne **move_intent_speed** cap, then add a small bonus push when `gain > 1`:
- `move_intent_speed_air = move_intent_speed_base * gain`
- `bonus = (gain - 1) * bonus_scale * air_accel * dt * move_intent_speed_base`
- `vel_planar += move_intent_dir * bonus`
This removes a hard top speed while keeping growth gradual and tied to input magnitude. Shaping remains optional but should be explicit and testable.

Air resistance should scale with speed and cap:
- `resistance_scale = saturate(speed / air_resistance_speed_scale)`
- `effective_resistance = air_resistance * resistance_scale`

Tunables:
- `theta_target`
- `g_min`, `g_peak`
- `bonus_scale`
- `blend_speed_start`, `blend_speed_end`

---

## 8) Tick order (arena)
Suggested order each tick:

1) Build Move Intent (`move_intent_dir`, `move_intent_mag`, `move_intent_speed`)
2) Update jump buffer state
3) Determine `request_jump` (grounded + jump pressed/held/buffer rules)
4) Apply ground friction (unless suppressed by frictionless jump policy)
5) Apply acceleration:
   - if grounded: ground accel toward `move_intent_speed`
   - if air: air accel toward `move_intent_speed` (with Golden Angle gain scaling)
6) Apply corridor shaping (primarily in air; modest)
7) Emit output to `character_collision` / KCC:
   - desired displacement or desired velocity for this tick
   - jump request (so collision layer can apply vertical impulse/velocity as designed)

**Note:** whether jump is represented as direct vertical velocity set, impulse, or KCC “up” motion is an integration detail owned by the collision/controller layer; the motor only issues `request_jump`.

---

## 9) Acceptance tests (arena)
Required harness courses:
1) Crisp stop
2) Crisp redirect
3) Air-strafe speed build
4) Doorway-at-speed line course
5) Frictionless jump retention (soft): repeated landings with held/buffered jump and measurable speed retention
6) Grounded stability (no flicker-induced jitter)

Each test should be runnable via input trace replay.

---

## 10) Differentiation and provenance
- All terminology in this spec is engine-owned (Golden Angle, move intent, frictionless jump).
- Collision behavior is Rapier KCC; the motor never implements step/slide algorithms.
- Tuning values must be derived empirically via your harness rather than copied from third-party sources.
