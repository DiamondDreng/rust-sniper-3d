//! Shared player movement. No ECS access, so the server runs it for real and the client
//! runs the exact same code for prediction.
//!
//! Movement model (Half-Life / Counter-Strike style):
//! - On the ground you *accelerate* toward your wish speed and *friction* slows you down,
//!   so you don't snap to full speed.
//! - In the air there's no friction, and air acceleration is capped per tick on the component
//!   of speed along your wish direction. Strafing while turning the view lets you gain speed.
//! - Jump must be pressed fresh each time (no holding space). A short buffer lets you press
//!   a few ticks before landing. The tick you jump has no friction, which is what makes
//!   bunny hopping keep your speed.
//! - Crouch pressed while moving fast on the ground starts a slide: a speed boost, very low
//!   friction, weak steering, and you can jump out of it. Crouched = lower eye and hitbox.

use bevy::prelude::*;

use crate::combat::FIRE_COOLDOWN_TICKS;
use crate::protocol::{InputFrame, PlayerState, TICK_DT};

/// Eye height above the feet.
pub const EYE_HEIGHT: f32 = 1.7;
pub const CROUCH_EYE_HEIGHT: f32 = 0.8;
const STAND_HALF_Y: f32 = 0.9;
const CROUCH_HALF_Y: f32 = 0.5;
pub const BODY_HALF_XZ: f32 = 0.4;

pub fn eye_height(crouched: bool) -> f32 {
    if crouched { CROUCH_EYE_HEIGHT } else { EYE_HEIGHT }
}

pub fn body_half_y(crouched: bool) -> f32 {
    if crouched { CROUCH_HALF_Y } else { STAND_HALF_Y }
}

/// Vertical distance from the eye down to the center of the body box.
pub fn eye_to_center(crouched: bool) -> f32 {
    eye_height(crouched) - body_half_y(crouched)
}

const MOVE_SPEED: f32 = 6.0;
const CROUCH_SPEED: f32 = 2.5;
const GROUND_ACCEL: f32 = 10.0;
const AIR_ACCEL: f32 = 14.0;
/// In the air, only this much speed along your wish direction counts toward the limit.
/// This small cap is what makes air strafing work the way it does in Half-Life.
/// Bigger = faster speed gain when strafing and turning (it was 0.7, which felt weak).
const AIR_WISH_CAP: f32 = 1.2;
const FRICTION: f32 = 6.0;
/// Below this speed friction acts as if you were this fast, so you stop instead of creeping.
const STOP_SPEED: f32 = 1.9;
/// Hard safety limit on horizontal speed (bhop could otherwise grow without bound).
const MAX_HORIZONTAL_SPEED: f32 = 30.0;
const GRAVITY: f32 = 25.0;
const JUMP_SPEED: f32 = 8.0;
/// Ticks a jump press is remembered (8 ticks = 125 ms at 64 Hz). Press jump up to this long
/// before landing and you still hop without touching the ground's friction.
const JUMP_BUFFER_TICKS: u8 = 8;

const SLIDE_MIN_SPEED: f32 = 5.0;
/// A slide multiplies your speed by this, but never starts you slower than `SLIDE_MIN_BOOSTED_SPEED`.
const SLIDE_BOOST_MULTIPLIER: f32 = 1.35;
const SLIDE_MIN_BOOSTED_SPEED: f32 = 10.0;
const SLIDE_END_SPEED: f32 = 2.5;
const SLIDE_TICKS: u16 = 80; // 1.25 s
const SLIDE_COOLDOWN: u16 = 40; // extra wait after a slide ends, so slides can't be chained for free speed
const SLIDE_FRICTION: f32 = 0.6;
const SLIDE_STEER_ACCEL: f32 = 3.0;

/// Converts movement keys + yaw into a unit-length (or zero) ground-plane direction.
pub fn wish_direction(yaw: f32, forward: f32, right: f32) -> Vec3 {
    let rotation = Quat::from_rotation_y(yaw);
    let forward_dir = rotation * Vec3::NEG_Z;
    let right_dir = rotation * Vec3::X;
    (forward_dir * forward + right_dir * right).normalize_or_zero()
}

fn overlaps(c1: Vec3, h1: Vec3, c2: Vec3, h2: Vec3) -> bool {
    (c1 - c2).abs().cmplt(h1 + h2).all()
}

/// The first solid box overlapping a player whose eye is at `eye`.
fn blocked(eye: Vec3, crouched: bool, solids: &[(Vec3, Vec3)]) -> Option<(Vec3, Vec3)> {
    let half = Vec3::new(BODY_HALF_XZ, body_half_y(crouched), BODY_HALF_XZ);
    let center = eye - Vec3::Y * eye_to_center(crouched);
    solids
        .iter()
        .find(|(c, h)| overlaps(center, half, *c, *h))
        .copied()
}

/// Ground friction on a horizontal velocity.
fn apply_friction(horizontal: &mut Vec3, friction: f32, dt: f32) {
    let speed = horizontal.length();
    if speed < 0.001 {
        *horizontal = Vec3::ZERO;
        return;
    }
    let drop = speed.max(STOP_SPEED) * friction * dt;
    let new_speed = (speed - drop).max(0.0);
    *horizontal *= new_speed / speed;
}

/// Ground acceleration: speed up toward `wish_speed` along `wish_dir`.
fn accelerate(horizontal: &mut Vec3, wish_dir: Vec3, wish_speed: f32, accel: f32, dt: f32) {
    let current = horizontal.dot(wish_dir);
    let add = wish_speed - current;
    if add <= 0.0 {
        return;
    }
    let accel_speed = (accel * wish_speed * dt).min(add);
    *horizontal += wish_dir * accel_speed;
}

/// Air acceleration: like `accelerate`, but the speed limit is tiny, so you can add sideways
/// speed while turning even when already moving much faster than `wish_speed`.
fn air_accelerate(horizontal: &mut Vec3, wish_dir: Vec3, wish_speed: f32, accel: f32, dt: f32) {
    let capped = wish_speed.min(AIR_WISH_CAP);
    let current = horizontal.dot(wish_dir);
    let add = capped - current;
    if add <= 0.0 {
        return;
    }
    let accel_speed = (accel * wish_speed * dt).min(add);
    *horizontal += wish_dir * accel_speed;
}

/// Applies one input frame (exactly one tick) to a player.
/// The server uses this for the real simulation; the client uses the very same
/// function for prediction and for replaying unconfirmed inputs.
///
/// Returns `true` if the rifle fired on this tick.
pub fn step_player(state: &mut PlayerState, frame: &InputFrame, solids: &[(Vec3, Vec3)]) -> bool {
    let dt = TICK_DT;
    state.cooldown = state.cooldown.saturating_sub(1);
    state.slide_cooldown = state.slide_cooldown.saturating_sub(1);

    // Dead players can't move or shoot, but still advance (gravity, sequence numbers).
    let alive = state.alive;

    // Fresh presses (rising edges) of jump and crouch.
    let jump_pressed = frame.jump && !state.jump_held;
    let crouch_pressed = frame.crouch && !state.crouch_held;
    state.jump_held = frame.jump;
    state.crouch_held = frame.crouch;
    state.jump_buffer = if jump_pressed {
        JUMP_BUFFER_TICKS
    } else {
        state.jump_buffer.saturating_sub(1)
    };

    // ---- Crouch / slide transitions ----
    let want_crouch = alive && frame.crouch;
    let speed_before = Vec2::new(state.velocity.x, state.velocity.z).length();

    if !state.crouched {
        // Only start crouching on the ground (keeps air logic simple).
        if want_crouch && state.grounded {
            state.crouched = true;
            // Feet stay where they are; the eye drops.
            state.position.y -= EYE_HEIGHT - CROUCH_EYE_HEIGHT;

            if crouch_pressed && speed_before >= SLIDE_MIN_SPEED && state.slide_cooldown == 0 {
                state.slide_ticks = SLIDE_TICKS;
                state.slide_cooldown = SLIDE_TICKS + SLIDE_COOLDOWN;
                let direction = Vec3::new(state.velocity.x, 0.0, state.velocity.z) / speed_before;
                let boosted = (speed_before * SLIDE_BOOST_MULTIPLIER).max(SLIDE_MIN_BOOSTED_SPEED);
                state.velocity.x = direction.x * boosted;
                state.velocity.z = direction.z * boosted;
            }
        }
    } else if !want_crouch {
        // Stand up, but only if there's room above us.
        let standing = state.position + Vec3::Y * (EYE_HEIGHT - CROUCH_EYE_HEIGHT);
        if blocked(standing, false, solids).is_none() {
            state.crouched = false;
            state.position = standing;
            state.slide_ticks = 0;
        }
    }

    // ---- Jump ----
    // Happens before friction: once airborne, friction no longer applies this tick.
    if alive && state.jump_buffer > 0 && state.grounded {
        state.velocity.y = JUMP_SPEED;
        state.grounded = false;
        state.jump_buffer = 0;
        state.slide_ticks = 0; // jumping ends a slide but keeps your speed
    }

    // ---- Horizontal movement ----
    let wish_dir = if alive {
        wish_direction(frame.yaw, frame.forward, frame.right)
    } else {
        Vec3::ZERO
    };
    let wish_speed = if state.crouched { CROUCH_SPEED } else { MOVE_SPEED };
    let sliding = state.crouched && state.slide_ticks > 0;

    let mut horizontal = Vec3::new(state.velocity.x, 0.0, state.velocity.z);
    if state.grounded {
        if sliding {
            apply_friction(&mut horizontal, SLIDE_FRICTION, dt);
            // Weak steering only.
            air_accelerate(&mut horizontal, wish_dir, wish_speed, SLIDE_STEER_ACCEL, dt);
            state.slide_ticks -= 1;
            if horizontal.length() < SLIDE_END_SPEED {
                state.slide_ticks = 0;
            }
        } else {
            apply_friction(&mut horizontal, FRICTION, dt);
            accelerate(&mut horizontal, wish_dir, wish_speed, GROUND_ACCEL, dt);
        }
    } else {
        air_accelerate(&mut horizontal, wish_dir, wish_speed, AIR_ACCEL, dt);
    }

    let speed = horizontal.length();
    if speed > MAX_HORIZONTAL_SPEED {
        horizontal *= MAX_HORIZONTAL_SPEED / speed;
    }
    state.velocity.x = horizontal.x;
    state.velocity.z = horizontal.z;
    state.velocity.y -= GRAVITY * dt;

    // ---- Collision ----
    let crouched = state.crouched;
    let half_y = body_half_y(crouched);
    let e2c = eye_to_center(crouched);

    // Move one axis at a time so we slide along walls instead of sticking.
    state.position.x += state.velocity.x * dt;
    if blocked(state.position, crouched, solids).is_some() {
        state.position.x -= state.velocity.x * dt;
        state.velocity.x = 0.0;
    }
    state.position.z += state.velocity.z * dt;
    if blocked(state.position, crouched, solids).is_some() {
        state.position.z -= state.velocity.z * dt;
        state.velocity.z = 0.0;
    }

    state.grounded = false;
    state.position.y += state.velocity.y * dt;
    if let Some((c, h)) = blocked(state.position, crouched, solids) {
        if state.velocity.y <= 0.0 {
            // Landed on top of a box.
            state.position.y = c.y + h.y + half_y + e2c;
            state.grounded = true;
        } else {
            // Bumped our head.
            state.position.y = c.y - h.y - half_y + e2c;
        }
        state.velocity.y = 0.0;
    }

    // Floor at y = 0.
    let eye_above_feet = eye_height(crouched);
    if state.position.y - eye_above_feet <= 0.0 {
        state.position.y = eye_above_feet;
        state.velocity.y = state.velocity.y.max(0.0);
        state.grounded = true;
    }

    // ---- Look + shoot ----
    state.yaw = frame.yaw;
    state.pitch = frame.pitch;
    state.last_seq = frame.seq;

    let fired = alive && frame.fire && state.cooldown == 0;
    if fired {
        state.cooldown = FIRE_COOLDOWN_TICKS;
    }
    fired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standing_on_floor() -> PlayerState {
        PlayerState {
            position: Vec3::new(0.0, EYE_HEIGHT, 0.0),
            alive: true,
            grounded: true,
            ..default()
        }
    }

    fn frame(seq: u32) -> InputFrame {
        InputFrame {
            seq,
            ..default()
        }
    }

    fn horizontal_speed(state: &PlayerState) -> f32 {
        Vec2::new(state.velocity.x, state.velocity.z).length()
    }

    #[test]
    fn ground_movement_accelerates_instead_of_snapping() {
        let mut state = standing_on_floor();
        let input = InputFrame {
            forward: 1.0,
            ..frame(1)
        };
        step_player(&mut state, &input, &[]);
        let speed = horizontal_speed(&state);
        assert!(speed > 0.1 && speed < MOVE_SPEED * 0.5, "speed was {speed}");

        for seq in 2..200 {
            step_player(&mut state, &InputFrame { forward: 1.0, ..frame(seq) }, &[]);
        }
        assert!((horizontal_speed(&state) - MOVE_SPEED).abs() < 0.05);
    }

    #[test]
    fn friction_brings_you_to_a_stop() {
        let mut state = standing_on_floor();
        state.velocity = Vec3::new(0.0, 0.0, -MOVE_SPEED);
        for seq in 1..200 {
            step_player(&mut state, &frame(seq), &[]);
        }
        assert!(horizontal_speed(&state) < 0.01);
    }

    #[test]
    fn jumping_keeps_horizontal_speed_because_there_is_no_friction() {
        let mut state = standing_on_floor();
        state.velocity = Vec3::new(0.0, 0.0, -8.0);
        step_player(&mut state, &InputFrame { jump: true, ..frame(1) }, &[]);
        assert!(!state.grounded);
        assert!(state.velocity.y > 0.0);
        assert!(horizontal_speed(&state) > 7.99, "speed was {}", horizontal_speed(&state));
    }

    #[test]
    fn holding_jump_does_not_auto_hop() {
        let mut state = standing_on_floor();
        // Hold jump for many ticks: only the first press may produce a jump.
        let mut jumps = 0;
        let mut was_grounded = true;
        for seq in 1..400 {
            step_player(&mut state, &InputFrame { jump: true, ..frame(seq) }, &[]);
            if was_grounded && !state.grounded {
                jumps += 1;
            }
            was_grounded = state.grounded;
        }
        assert_eq!(jumps, 1);
    }

    #[test]
    fn air_strafing_gains_speed() {
        let mut state = standing_on_floor();
        state.grounded = false;
        state.position.y = 20.0; // high up, so we stay airborne
        state.velocity = Vec3::new(0.0, 0.0, -8.0);
        let before = horizontal_speed(&state);
        // Real air strafing: hold strafe-right and keep turning the view so the wish direction
        // stays perpendicular to where we're moving. Each tick adds a little sideways speed, and
        // because it's perpendicular, it adds to the total instead of just redirecting it.
        // (Strafing *without* turning gains only one tiny bit, then stops: sideways speed
        // reaches the air cap and nothing more can be added.)
        for seq in 1..60 {
            let moving = Vec3::new(state.velocity.x, 0.0, state.velocity.z).normalize();
            let perpendicular = Vec3::new(-moving.z, 0.0, moving.x);
            // wish_direction(yaw, 0, 1) points along (cos yaw, 0, -sin yaw); solve for yaw.
            let yaw = (-perpendicular.z).atan2(perpendicular.x);
            step_player(
                &mut state,
                &InputFrame {
                    right: 1.0,
                    yaw,
                    ..frame(seq)
                },
                &[],
            );
        }
        assert!(
            horizontal_speed(&state) > before + 0.5,
            "speed went from {before} to {}",
            horizontal_speed(&state)
        );
    }

    #[test]
    fn crouching_while_fast_starts_a_slide_with_a_boost() {
        let mut state = standing_on_floor();
        state.velocity = Vec3::new(0.0, 0.0, -6.0);
        step_player(&mut state, &InputFrame { crouch: true, ..frame(1) }, &[]);
        assert!(state.crouched);
        assert!(state.slide_ticks > 0);
        // Running at 6 m/s: the slide should clearly boost it (to about 10 m/s).
        assert!(horizontal_speed(&state) > 9.5, "speed was {}", horizontal_speed(&state));
        assert!((state.position.y - CROUCH_EYE_HEIGHT).abs() < 0.01);
    }

    #[test]
    fn crouching_while_slow_does_not_slide() {
        let mut state = standing_on_floor();
        step_player(&mut state, &InputFrame { crouch: true, ..frame(1) }, &[]);
        assert!(state.crouched);
        assert_eq!(state.slide_ticks, 0);
    }

    #[test]
    fn cannot_stand_up_under_a_low_ceiling() {
        let mut state = standing_on_floor();
        state.crouched = true;
        state.position.y = CROUCH_EYE_HEIGHT;
        // A slab whose underside is 1.2 m above the floor: too low to stand (1.8 m), fine to crouch (1.0 m).
        let ceiling = (Vec3::new(0.0, 1.7, 0.0), Vec3::new(5.0, 0.5, 5.0));
        step_player(&mut state, &frame(1), &[ceiling]);
        assert!(state.crouched);
    }
}
