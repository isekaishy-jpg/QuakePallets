# Player Movement Spec — RPG Motor v1 (Rapier KCC)

## Units and conventions
- **Distance:** meters (world units are meters)
- **Time:** seconds
- **Angles:** radians in code; degrees are allowed in authored config (e.g., TOML) and must be converted at load
- **Velocity:** m/s
- **Acceleration:** m/s²
- **Gravity:** m/s² (project default should be stated in the controller/collision layer)

Rapier is unitless; this project standardizes on the scale above for numeric stability and consistency.


## Purpose
Define a **standard MMO/RPG-style** character movement motor suitable for:
- predictable navigation,
- stable slope/step traversal,
- conservative air behavior,
- straightforward tuning.

This spec is motor-only. Collision, stepping, and grounded detection are delegated to **Rapier Kinematic Character Controller (KCC)** via `character_collision`.

---

## 1) Goals (RPG)
0. **Normal MMO movement**: strafe at full speed; only backpedal is slower.
1. **Predictable speed control**: consistent ramp-up/ramp-down; no “snappy” FPS redirects by default.
2. **Stable grounding**: minimal jitter on slopes/steps; strong ground adherence.
3. **Conservative air behavior**: limited or no air steering; no momentum retention mechanics.
4. **Mode simplicity**: small set of knobs; defaults should “feel normal” without special tricks.
5. **Clean-room posture**: engine-owned terminology; no third-party movement naming.

---

## 2) System boundaries
### 2.1 RPG motor responsibilities
- Convert input + view yaw into **Move Intent**.
- Produce a desired planar velocity that follows intent with configurable smoothing.
- Decide jump requests (optional; conservative).
- Apply basic ground friction / braking policy (non-skill-based).

### 2.2 Not in scope (owned by collision/controller)
- stepping and slide resolution (Rapier KCC)
- depenetration, ground snap, slope limits
- vertical motion integration (jump, gravity)

---

## 3) Data model

### 3.1 Movement state (RPG-relevant subset)
- `pos: vec3`
- `vel: vec3`
- `view_yaw_rad: float`
- `is_grounded: bool` (from KCC)
- `ground_normal: vec3` (from KCC when grounded)

### 3.2 Input (per tick)
- `move_forward` in [-1, +1]
- `move_strafe`  in [-1, +1]
- `jump_pressed` (edge) (optional, if your RPG mode allows jumping)
- `dt` (fixed tick recommended)

---

## 4) Move Intent (engine-owned)

RPG motor uses the same **Move Intent** concept as arena:
- `move_intent_dir`: unit vector (world-space, planar or ground-tangent)
- `move_intent_mag`: scalar in [0, 1]
- `move_intent_speed`: `move_intent_mag * max_speed`

### 4.1 Direction computation
Compute yaw-relative planar intent:
- `intent2 = forward2 * move_forward + right2 * move_strafe`
- `move_intent_mag = clamp(length(intent2), 0, 1)`
- `move_intent_dir_planar = normalize(intent2)` if mag > 0 else (0,0)

When grounded, project onto the ground tangent plane (recommended) to avoid “into the slope” artifacts.

---

## 5) Core behavior: target-velocity tracking

RPG motor is defined by tracking a **target planar velocity** with configurable rise/fall rates.

### 5.1 Target planar velocity
- `target_vel = move_intent_dir * move_intent_speed`

Normal MMO scaling rules:
- `backpedal_speed_scale` applies when moving backward (forward<0), typically 0.5–0.8.
- Strafing uses **full speed** (no strafe penalty): `strafe_speed_scale = 1.0`.

These apply by scaling `move_intent_speed` based on input direction.

### 5.2 Approach target smoothly (time-constant model)
Use separate time constants for speeding up vs slowing down:

- `tau_accel` (seconds) when `|target_vel| > |vel_planar|`
- `tau_decel` (seconds) when `|target_vel| <= |vel_planar|`

Update:
- `alpha = 1 - exp(-dt / tau)` where `tau = tau_accel or tau_decel`
- `vel_planar = lerp(vel_planar, target_vel, alpha)`

This produces “normal MMO movement” feel: predictable and non-twitchy.

### 5.3 Hard caps and braking
- Clamp planar speed to `max_speed` after update.
- If `move_intent_mag` is zero, allow a slightly faster stop via:
  - using `tau_decel_stop` (optional) or simply a smaller `tau_decel`.

Avoid stop_speed floors and high friction tricks used in arena mode.

---

## 6) Air behavior (conservative)
Default RPG policy:
- If airborne, continue integrating planar velocity with **very weak** tracking (or none).

Two standard options (pick one as default; both are “normal”):
- **Option A (recommended):** no air steering  
  - When airborne, do not adjust planar velocity toward target; only clamp if needed.
- **Option B:** limited air steering  
  - Apply target tracking with a much larger `tau_air` (slow response).

Do not implement any momentum retention mechanics (no frictionless jump, no angle gain).

---

## 7) Jumping (optional, conservative)
Many MMO/RPG movement sets allow jumping but treat it as purely vertical with minimal gameplay impact.

Motor behavior:
- If `jump_enabled` and grounded and `jump_pressed`, set `request_jump = true`.
- No buffering by default.

Vertical motion contract is owned by controller/collision layer (see §9.1).

---

## 8) Tick order (RPG)
1) Build Move Intent (`move_intent_dir/mag/speed`), apply backpedal/strafe scales.
2) Determine `target_vel`.
3) Choose time constant (`tau_accel`/`tau_decel` and `tau_air` if airborne).
4) Update `vel_planar` toward `target_vel`.
5) Clamp to caps.
6) Determine `request_jump` (if enabled).
7) Emit `vel` + `request_jump`.

---

## 9) Parameters (initial baselines)

### 9.1 Collision profile coupling (Rapier KCC)
RPG feel is highly sensitive to KCC config:
- stronger ground snap than arena
- conservative step height
- conservative slope limit
- capsule size may be larger than arena (per profile)

### 9.2 RPG motor parameter table

#### A) Speed
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `max_speed` | Top planar speed | designer-defined | n/a |
| `backpedal_speed_scale` | Speed multiplier when moving backward | `0.65` | `0.5 – 0.8` |
| `strafe_speed_scale` | Speed multiplier when strafing | `1.0` | `1.0` |

#### B) Smoothing / responsiveness
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `tau_accel` | Time constant to ramp up speed (sec) | `0.18` | `0.10 – 0.35` |
| `tau_decel` | Time constant to ramp down speed (sec) | `0.14` | `0.08 – 0.30` |
| `tau_air` | Air steering time constant (sec), if enabled | `0.60` | `0.40 – 1.20` |

Notes:
- Smaller tau ⇒ snappier. Larger tau ⇒ heavier.
- If you want “WoW-ish” normalcy, keep tau values in the ~0.12–0.25 sec neighborhood.

#### C) Air policy
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `air_policy` | `none` or `limited` | `none` | n/a |

If `air_policy == limited`, apply tracking with `tau_air`. Otherwise preserve planar velocity in air.

#### D) Jumping
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `jump_enabled` | Allow jump input | `true` | n/a |

Vertical parameters (`jump_speed`, `gravity`) are owned by the controller/collision layer.

### 9.3 Recommended tuning order
1) Set capsule + KCC grounding (snap, slope, step) until traversal is stable.
2) Tune `max_speed`.
3) Tune `tau_accel` and `tau_decel` for “weight.”
4) Decide air policy (`none` recommended) and tune `tau_air` only if needed.
5) Decide on backpedal/strafe scales.

---

## 10) Acceptance tests (RPG)
1) **Predictable stop distance** from full speed with input release.
2) **Consistent ramp-up** time to 90% max speed.
3) **Slope/step stability**: no jitter or micro-hops while walking up/down steps and ramps.
4) **No unintended air control** (if `air_policy == none`).
5) **Full-speed strafe**: strafing reaches the same max speed as forward movement; backpedal is reduced by configuration.

All tests should be runnable via input trace replay.

---

## 11) Differentiation and provenance
- Engine-owned terminology only (Move Intent).
- No momentum retention mechanics (no Golden Angle gain, no frictionless jump).
- Behavior is defined via this spec + harness, not by external implementations.
