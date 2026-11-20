//! Shooting rules and hit detection. Pure functions: no ECS access, easy to test.

use bevy::prelude::*;

use crate::player::eye_height;

pub const MAX_HEALTH: f32 = 100.0;
/// Two body shots to kill, one headshot to kill.
pub const BODY_DAMAGE: f32 = 70.0;
pub const HEAD_DAMAGE: f32 = 100.0;
pub const RANGE: f32 = 500.0;
/// Bolt action: 1.5 s at 64 ticks per second.
pub const FIRE_COOLDOWN_TICKS: u16 = 96;
pub const RESPAWN_TICKS: u32 = 64 * 3;
/// Rounds in a full magazine. The rifle reloads automatically when empty.
pub const MAGAZINE_SIZE: u8 = 5;
pub const RELOAD_TICKS: u16 = 64 * 5 / 2; // 2.5 s
/// Freshly (re)spawned players can't be damaged for this long.
pub const SPAWN_PROTECTION_TICKS: u32 = 64 * 2;
/// First player to reach this many kills wins the match.
pub const SCORE_LIMIT: u16 = 10;
/// How long the winner screen stays up before the next match starts.
pub const INTERMISSION_TICKS: u32 = 64 * 10;
/// The server never rewinds further than this (500 ms), so high ping can't be abused.
pub const MAX_REWIND_TICKS: u32 = 32;

// Hitboxes, relative to a player's eye position.
// The visible body is a capsule (radius 0.4) whose top is 0.1 m above the eye. Hitboxes must
// stay *inside* that shape, or players get hit where they visibly aren't.
// Torso/legs box spans feet to 0.3 m below the eye; the head sphere sits on top of it.
// Crouching/sliding lowers the eye, and the whole hitbox with it.
const BODY_HALF_XZ: f32 = 0.35;
const HEAD_OFFSET: Vec3 = Vec3::new(0.0, -0.1, 0.0);
const HEAD_RADIUS: f32 = 0.2; // top of the sphere = eye + 0.1 = top of the capsule

/// Direction a player at (yaw, pitch) is aiming. Must match how the camera is rotated.
pub fn aim_direction(yaw: f32, pitch: f32) -> Vec3 {
    Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0) * Vec3::NEG_Z
}

/// Ray vs axis-aligned box. Returns distance along the ray to the entry point.
pub fn ray_aabb(origin: Vec3, dir: Vec3, center: Vec3, half: Vec3) -> Option<f32> {
    let min = center - half;
    let max = center + half;
    let inv = Vec3::ONE / dir;
    let t1 = (min - origin) * inv;
    let t2 = (max - origin) * inv;
    let t_near = t1.min(t2).max_element();
    let t_far = t1.max(t2).min_element();
    if t_far < 0.0 || t_near > t_far {
        None
    } else {
        Some(t_near.max(0.0))
    }
}

/// Ray vs sphere. Returns distance along the ray to the entry point.
pub fn ray_sphere(origin: Vec3, dir: Vec3, center: Vec3, radius: f32) -> Option<f32> {
    let to_origin = origin - center;
    let b = to_origin.dot(dir);
    let c = to_origin.length_squared() - radius * radius;
    let discriminant = b * b - c;
    if discriminant < 0.0 {
        return None;
    }
    let root = discriminant.sqrt();
    let near = -b - root;
    let far = -b + root;
    if far < 0.0 { None } else { Some(near.max(0.0)) }
}

/// A living player to test the shot against, at their (possibly rewound) position.
pub struct Target {
    pub id: Entity,
    /// Where their feet were.
    pub feet: Vec3,
    pub crouched: bool,
}

pub struct Hit {
    pub target: Entity,
    pub headshot: bool,
}

pub struct ShotResult {
    /// Where the shot ended: a wall, a player, or max range.
    pub end_distance: f32,
    pub hit: Option<Hit>,
}

/// Fires a ray. Walls block shots; the nearest player in front of the nearest wall is hit.
pub fn trace(
    origin: Vec3,
    dir: Vec3,
    walls: &[(Vec3, Vec3)],
    targets: &[Target],
) -> ShotResult {
    let mut wall_distance = RANGE;
    for (center, half) in walls {
        if let Some(t) = ray_aabb(origin, dir, *center, *half) {
            wall_distance = wall_distance.min(t);
        }
    }

    let mut best: Option<(f32, Hit)> = None;
    for target in targets {
        let eye = target.feet + Vec3::Y * eye_height(target.crouched);
        // Body box runs from the feet up to 0.3 m below the eye.
        let body_half_y = (eye_height(target.crouched) - 0.3) / 2.0;
        let body_center = target.feet + Vec3::Y * body_half_y;
        let body_half = Vec3::new(BODY_HALF_XZ, body_half_y, BODY_HALF_XZ);

        let body = ray_aabb(origin, dir, body_center, body_half);
        let head = ray_sphere(origin, dir, eye + HEAD_OFFSET, HEAD_RADIUS);
        let candidate = match (head, body) {
            (Some(h), Some(b)) if h <= b => Some((h, true)),
            (Some(_), Some(b)) => Some((b, false)),
            (Some(h), None) => Some((h, true)),
            (None, Some(b)) => Some((b, false)),
            (None, None) => None,
        };
        if let Some((t, headshot)) = candidate {
            if t < wall_distance && best.as_ref().is_none_or(|(best_t, _)| t < *best_t) {
                best = Some((
                    t,
                    Hit {
                        target: target.id,
                        headshot,
                    },
                ));
            }
        }
    }

    match best {
        Some((t, hit)) => ShotResult {
            end_distance: t,
            hit: Some(hit),
        },
        None => ShotResult {
            end_distance: wall_distance,
            hit: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::{CROUCH_EYE_HEIGHT, EYE_HEIGHT};

    fn standing_target(z: f32) -> Target {
        Target {
            id: Entity::PLACEHOLDER,
            feet: Vec3::new(0.0, 0.0, z),
            crouched: false,
        }
    }

    #[test]
    fn ray_hits_box_in_front_and_misses_behind() {
        let center = Vec3::new(0.0, 0.0, -10.0);
        let half = Vec3::splat(1.0);
        let t = ray_aabb(Vec3::ZERO, Vec3::NEG_Z, center, half).unwrap();
        assert!((t - 9.0).abs() < 1e-4);
        assert!(ray_aabb(Vec3::ZERO, Vec3::Z, center, half).is_none());
    }

    #[test]
    fn ray_hits_sphere_and_misses_to_the_side() {
        let center = Vec3::new(0.0, 0.0, -10.0);
        assert!(ray_sphere(Vec3::ZERO, Vec3::NEG_Z, center, 0.5).is_some());
        assert!(ray_sphere(Vec3::new(5.0, 0.0, 0.0), Vec3::NEG_Z, center, 0.5).is_none());
    }

    #[test]
    fn wall_blocks_shot_to_player_behind_it() {
        let target = standing_target(-20.0);
        let wall = (Vec3::new(0.0, 1.7, -10.0), Vec3::new(5.0, 5.0, 0.5));
        let origin = Vec3::new(0.0, EYE_HEIGHT, 0.0);
        let blocked = trace(origin, Vec3::NEG_Z, &[wall], &[target]);
        assert!(blocked.hit.is_none());
    }

    #[test]
    fn aiming_just_above_the_visible_head_misses() {
        let target = standing_target(-20.0);
        // The visible capsule ends 0.1 m above the eye. 0.15 m above must be a clean miss.
        let origin = Vec3::new(0.0, EYE_HEIGHT + 0.15, 0.0);
        let result = trace(origin, Vec3::NEG_Z, &[], &[target]);
        assert!(result.hit.is_none());
    }

    #[test]
    fn crouched_player_cannot_be_hit_at_standing_head_height() {
        let target = Target {
            crouched: true,
            ..standing_target(-20.0)
        };
        // A standing player's head would be here, but a crouched player's top is ~1.0 m.
        let origin = Vec3::new(0.0, EYE_HEIGHT, 0.0);
        let result = trace(origin, Vec3::NEG_Z, &[], &[target]);
        assert!(result.hit.is_none());
    }

    #[test]
    fn crouched_player_can_be_headshot_at_crouched_eye_height() {
        let target = Target {
            crouched: true,
            ..standing_target(-20.0)
        };
        let origin = Vec3::new(0.0, CROUCH_EYE_HEIGHT, 0.0);
        let result = trace(origin, Vec3::NEG_Z, &[], &[target]);
        assert!(result.hit.unwrap().headshot);
    }

    #[test]
    fn aiming_at_eye_height_is_a_headshot() {
        let target = standing_target(-20.0);
        let origin = Vec3::new(0.0, EYE_HEIGHT, 0.0);
        let result = trace(origin, Vec3::NEG_Z, &[], &[target]);
        assert!(result.hit.unwrap().headshot);
    }
}
