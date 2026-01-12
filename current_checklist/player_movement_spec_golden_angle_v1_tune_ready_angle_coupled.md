# Player Movement Spec — Golden Angle v1 (Arena Focus, Rapier KCC)

## Status
This document **supersedes** the prior QuakeWorld-oriented player physics spec by:
- removing all dependence on SlideMove/StepSlideMove and plane clipping,
- delegating collision/stepping/grounding to **Rapier Kinematic Character Controller (KCC)**,
- replacing legacy terminology (“wishdir”, “bhop”) with engine-owned terminology:
  - **Move Intent** (`move_intent_dir/mag/speed`)
  - **frictionless jump** (soft default)
  - **Golden Angle** (arena movement system name).

## Units and conventions
- **Distance:** meters (world units are meters)
- **Time:** seconds
- **Angles:** radians in code; degrees are allowed in authored config (e.g., TOML) and must be converted at load
- **Velocity:** m/s
- **Acceleration:** m/s²
- **Gravity:** m/s² (project default should be stated in the controller/collision layer)

Rapier is unitless; this project standardizes on the scale above for numeric stability and consistency.


---

## 1) Goals (Arena)
1. **Crisp ground movement**: decisive stop and strong redirects.
2. **Air-strafe speed build**: speed can be built in air through skillful inputs.
3. **High-speed navigation**: threading doorways/corridors at speed is practical without auto-steer.
4. **Jump-chain retention**: speed is not unintentionally bled on landings when chaining jumps (via *frictionless jump* policy).
5. **Clean-room posture**: no third-party step/slide collision algorithms; no third-party naming in shipped surfaces.

---

## 2) System boundaries (must not blur)
### 2.1 Motor (this spec)
The **Arena Motor** is responsible for:
- deriving move intent from input + view yaw,
- applying friction/accel policies,
- determining jump request based on input + buffer policy,
- producing a **desired planar velocity** (and jump intent) each tick.

The motor must **not**:
- perform collision queries,
- implement step logic,
- clip velocity against planes,
- depend on BSP semantics.

### 2.2 Collision/Controller (Rapier KCC)
The collision/controller layer is responsible for:
- capsule shape configuration (per profile),
- stepping and slide resolution via Rapier KCC,
- authoritative grounded/contact results,
- applying vertical motion for jumps according to project conventions.

---

## 3) Data model

### 3.1 Player movement state (arena-relevant subset)
- `pos: vec3`
- `vel: vec3`
- `view_yaw_rad: float`
- `is_grounded: bool` (from KCC)
- `ground_normal: vec3` (from KCC when grounded)
- `jump_buffer_ticks_remaining: int` (motor-owned)

### 3.2 Input (per tick)
- `move_forward` in [-1, +1]
- `move_strafe`  in [-1, +1]
- `jump_pressed` (edge)
- `jump_held` (level; can be derived)
- `dt` (fixed tick recommended)

---

## 4) Terminology: Move Intent (engine-owned)
Replace “wish direction/speed” with **Move Intent**.

- `move_intent_dir`: unit vector (world-space, planar or ground-tangent)
- `move_intent_mag`: scalar in [0, 1]
- `move_intent_speed`: `move_intent_mag * max_speed_{ground|air}`

Rationale:
- keeps direction stable for Golden Angle gain and corridor shaping,
- preserves a clean path for partial-input semantics (even if keyboard-only today).

---

## 5) Build Move Intent

### 5.1 Yaw-relative planar intent
Let:
- `forward2 = (cos(yaw), sin(yaw))`
- `right2   = (-sin(yaw), cos(yaw))`

Compute:
- `intent2 = forward2 * move_forward + right2 * move_strafe`
- `move_intent_mag = clamp(length(intent2), 0, 1)`
- `move_intent_dir_planar = normalize(intent2)` if mag > 0 else (0,0)

### 5.2 Ground-tangent projection (recommended when grounded)
When grounded:
- lift planar dir to 3D, then project onto the ground tangent plane:
  - `dir3_tangent = normalize(dir3 - n * dot(dir3, n))` where `n = ground_normal`
- use `dir3_tangent` as `move_intent_dir`

When airborne:
- use planar direction directly (lift to 3D with zero vertical component).

---

## 6) Motor primitives

### 6.1 Ground friction (crisp stop)
Apply friction when grounded unless suppressed by frictionless jump policy.

Tunables:
- `friction`
- `stop_speed`

Rule:
- `drop = max(speed, stop_speed) * friction * dt`
- scale planar velocity down by `(speed - drop) / speed` clamped to [0,1]

### 6.2 Planar accelerate (projection-limited)
Inputs:
- planar velocity `v`
- `move_intent_dir` (unit)
- `move_intent_speed`
- `accel` (ground or air)

Rule:
- `current_along = dot(v, dir)`
- `add = move_intent_speed - current_along`
- `push = min(accel * dt * move_intent_speed, add)` if `add > 0`
- `v += dir * push`

### 6.3 Corridor shaping (modest, constrained)
Goal: enable practical doorway/corridor line shaping at speed without free pivots.

This is a *constrained* direction blend/rotation:
- apply primarily in air
- require minimum speed and minimum alignment before assisting
- cap per-tick angular change

Tunables:
- `shaping_strength`
- `min_speed_for_shaping`
- `max_shaping_angle_per_tick`
- `min_alignment`

---

## 7) Jump buffering + frictionless jump (required for arena)

### 7.1 Jump buffering (required)
Policy:
- if `jump_pressed` occurs while not currently able to jump, start buffer timer.
- if player becomes grounded while buffer active, consume and issue `request_jump`.

Tunables:
- `jump_buffer_window_ticks`

### 7.2 Frictionless jump mode (required knob)
Expose:
- `frictionless_jump_mode = none | soft | hard` (arena default: **soft**)

### 7.3 Soft frictionless jump (arena default)
Soft mode is defined strictly as a **friction application rule**:

Recommended v1 rule:
- If `request_jump` is true this tick, **skip ground friction** this tick.
- Additionally, if grounded and (`jump_held` or buffered jump pending), apply a grace window:
  - `landing_grace_ticks`
  - `landing_friction_scale` in [0,1] (multiplies friction)
  - **Golden Angle coupling (new knob):** scale friction during the grace window based on Golden Angle “quality”:
    - better Golden Angle alignment ⇒ **less friction**
    - worse alignment ⇒ closer to the base `landing_friction_scale`

Implementation sketch (soft mode, during grace window):
- Compute `theta` between `vel_dir` and `view_forward_dir` (planar).
- Compute normalized Golden Angle quality:
  - `q = saturate(theta / theta_target)` (0 = straight-ahead, 1 = at/beyond target)
  - Optional smoothing: `q = q*q*(3 - 2*q)`
- Effective grace friction scale:
  - `landing_friction_scale_eff = lerp(landing_friction_scale, landing_friction_scale_best_angle, q)`
- Apply friction using `friction * landing_friction_scale_eff`.

To disable Golden Angle coupling without adding another knob:
- set `landing_friction_scale_best_angle == landing_friction_scale`.

Tunables:
- `landing_grace_ticks`
- `landing_friction_scale`
- `landing_friction_scale_best_angle` (new)
---

## 8) Golden Angle gain (arena signature mechanic)

### 8.1 Intent/velocity angle
Compute planar angle between:
- `vel_dir = normalize(vel_planar)` (if speed > eps)
- `view_forward_dir` (unit, yaw forward)

Let `theta = angle_between(vel_dir, view_forward_dir)`.

### 8.2 Gain curve + speed blend
Define a normalized quality ramp from straight-ahead to the target angle:

- `q = saturate(theta / theta_target)` (0 at straight-ahead, 1 at or beyond target)
- Optional smoothing: `q = q*q*(3 - 2*q)`
- `g(theta) = lerp(g_min, g_peak, q)`

Blend in by speed:
- `t = saturate((speed - blend_speed_start)/(blend_speed_end - blend_speed_start))`
- `gain = lerp(1.0, g(theta), t)`

Apply gain to:
- effective **air acceleration** and airborne **move_intent_speed** cap (to allow speed build):
  - `air_accel_eff = air_accel_base * gain`
- `move_intent_speed_air = move_intent_speed_base * gain`
  - when `gain > 1`, add a bonus push:
    - `bonus = (gain - 1) * bonus_scale * air_accel_base * dt * move_intent_speed_base`
    - `vel_planar += move_intent_dir * bonus`

Tunables:
- `theta_target`
- `g_min`, `g_peak`
- `blend_speed_start`, `blend_speed_end`
- `air_accel_base`

---

## 9) Tick order (arena)
At each fixed tick:

1) Build Move Intent (`move_intent_dir/mag/speed`)
2) Update jump buffer state
3) Determine `request_jump` (grounded + jump_pressed/held + buffer policy)
4) Apply ground friction (if grounded and not suppressed)
5) Apply acceleration:
   - grounded: ground accel toward `move_intent_speed`
   - airborne: air accel with Golden Angle gain scaling
6) Apply corridor shaping (primarily airborne)
7) Emit:
   - updated `vel`
   - `request_jump`
   - any debug metrics

Then the controller/collision layer:
- integrates desired motion with Rapier KCC,
- returns updated grounded/contact state for the next tick.

---

## 10) Parameters (initial baselines)

This section defines **implementation-ready knobs** with initial ranges. Values are expressed relative to:

- `W = max_speed_ground` (baseline ground top speed, **m/s**)
- fixed tick `dt` (recommended), so “per-second” parameters are applied via `* dt`

### 10.1 Collision profile coupling (Rapier KCC)
These are not motor parameters, but they materially affect feel and must be tuned alongside:
- capsule radius / height (arena profile)
- step height
- max slope angle
- ground snap distance

The motor assumes KCC delivers stable `is_grounded` and `ground_normal`.

### 10.2 Arena motor parameter table

#### A) Speed caps
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `max_speed_ground` | Ground move intent speed cap | `W` | designer-defined |
| `max_speed_air` | Air move intent speed cap | `W` | `0.9W – 1.1W` |

#### B) Ground responsiveness
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `ground_accel` | Planar accel rate when grounded | `14` | `10 – 25` |
| `friction` | Ground friction coefficient | `6` | `4 – 10` |
| `stop_speed` | Friction floor to make stopping crisp | `0.30W` | `0.25W – 0.40W` |

Notes:
- Higher `ground_accel` improves redirect responsiveness but can feel “snappy” if too high.
- `stop_speed` is the primary “crisp stop” dial; increase it before increasing friction if stop feels mushy.

#### C) Air acceleration + Golden Angle
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `air_accel_base` | Base planar accel rate in air (before gain) | `8` | `4 - 14` |
| `air_resistance` | Base air damping coefficient (speed-scaled) | `0.0` | `0.0 - 1.0` |
| `air_resistance_speed_scale` | Speed where air resistance reaches full strength | `2W` | `1W - 4W` |
| `theta_target` | Target offset angle between `vel_dir` and `view_forward_dir` | `35°` | `20° - 55°` |
| `g_min` | Minimum gain factor (at poor angles) | `0.85` | `0.70 - 1.00` |
| `g_peak` | Peak gain factor (near target angle) | `1.25` | `1.05 - 1.60` |
| `bonus_scale` | Scale for the uncapped bonus push when `gain > 1` | `0.5` | `0.1 - 1.0` |
| `blend_speed_start` | Speed where Golden Angle gain starts blending in | `1.0W` | `0.8W - 1.2W` |
| `blend_speed_end` | Speed where Golden Angle gain is fully applied | `1.4W` | `1.2W - 1.8W` |

Notes:
- Interpret `theta_target` as the “signature” feel knob: lower angles bias toward straight-line efficiency; higher angles bias toward intentional offset strafing.
- Keep `g_peak` modest early; raise only if speed build feels capped after frictionless jump tuning is correct.
- If the gain ramp feels too abrupt, lower `g_peak` or raise `theta_target`.
- If speed builds too quickly once uncapped, reduce `bonus_scale` before lowering `g_peak`.

#### D) Corridor shaping (air line shaping)
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `shaping_strength` | Rotation rate toward intent (radians/sec) | `1.5` | `0.0 – 3.0` |
| `min_speed_for_shaping` | Minimum planar speed before shaping applies | `0.9W` | `0.6W – 1.2W` |
| `max_shaping_angle_per_tick` | Hard cap on per-tick steering assistance | `2.0°` | `0.5° – 4.0°` |
| `min_alignment` | Require `dot(vel_dir, intent_dir)` above this to shape | `0.0` | `-0.2 – 0.5` |

Notes:
- Start conservative: shaping should assist “line holding,” not create free turns.
- If doorway-at-speed feels impossible, first increase `max_shaping_angle_per_tick` slightly, then `shaping_strength`.

#### E) Jump buffering + frictionless jump (required arena policies)
| Parameter | Meaning | Suggested start | Typical range |
|---|---|---:|---:|
| `jump_buffer_window_ticks` | Buffer duration | `3 ticks` | `2 – 6 ticks` |
| `frictionless_jump_mode` | `none|soft|hard` | `soft` | n/a |
| `landing_grace_ticks` | Extra window after landing where friction is reduced (soft mode) | `2 ticks` | `0 – 4 ticks` |
| `landing_friction_scale` | Multiply friction during grace window | `0.25` | `0.0 – 0.6` |
| `landing_friction_scale_best_angle` | Grace-window friction scale at best Golden Angle quality (lower = more “frictionless”) | `0.0` | `0.0 – landing_friction_scale` |

Notes:
- If speed bleeds on jump chains, reduce `landing_friction_scale` and/or increase `landing_grace_ticks` before increasing `g_peak`.
- If you want “skill-linked” retention (better Golden Angle alignment retains more speed), lower `landing_friction_scale_best_angle`.
- If speed runs away too easily, increase `landing_friction_scale` first (do not immediately nerf air accel).
### 10.3 Recommended tuning order
1) Get **ground feel** right: `ground_accel`, `friction`, `stop_speed`.
2) Enable and tune **frictionless jump (soft)** + `jump_buffer_window_ticks` until jump chains retain speed as intended.
3) Tune **air accel base** (without extreme gain).
4) Tune **Golden Angle** (`theta_target`, `g_min/g_peak`, blend speeds).
5) Add **corridor shaping** last, minimally, to satisfy doorway-at-speed without oversteer.


## 11) Acceptance tests (arena)
1) **Crisp stop**
2) **Crisp redirect**
3) **Air-strafe speed build**
4) **Doorway-at-speed**
5) **Frictionless jump retention (soft)**: repeated landings with held/buffered jump retains speed measurably
6) **Grounded stability**: no flicker-induced jitter

All tests should be runnable via input trace replay.

---

## 12) Notes on differentiation and provenance
- This spec uses only engine-owned terminology.
- Collision is Rapier KCC; motor does not implement third-party step/slide logic.
- Tuning values must be derived via your harness, not copied from external codebases.
