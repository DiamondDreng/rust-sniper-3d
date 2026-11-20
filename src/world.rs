//! The level layout, shared by client and server.
//!
//! The server only needs the collision boxes; the client also builds visuals from the
//! same list, so what you see always matches what you collide with.

use bevy::prelude::*;

use crate::player::EYE_HEIGHT;

/// Where players (re)spawn, as eye positions. All on open ground.
pub fn spawn_points() -> [Vec3; 8] {
    [
        Vec3::new(0.0, EYE_HEIGHT, 26.0),
        Vec3::new(0.0, EYE_HEIGHT, -26.0),
        Vec3::new(-24.0, EYE_HEIGHT, -24.0),
        Vec3::new(24.0, EYE_HEIGHT, -24.0),
        Vec3::new(-24.0, EYE_HEIGHT, 24.0),
        Vec3::new(24.0, EYE_HEIGHT, 24.0),
        Vec3::new(-14.0, EYE_HEIGHT, 10.0),
        Vec3::new(14.0, EYE_HEIGHT, -10.0),
    ]
}

pub struct LevelBox {
    pub center: Vec3,
    pub size: Vec3,
    pub color: Color,
}

pub fn level_boxes() -> Vec<LevelBox> {
    let wall = Color::srgb(0.5, 0.5, 0.55);
    let cover = Color::srgb(0.7, 0.3, 0.3);

    let mut boxes = Vec::new();

    // Outer walls
    for (center, size) in [
        (Vec3::new(0.0, 2.0, -30.5), Vec3::new(62.0, 4.0, 1.0)),
        (Vec3::new(0.0, 2.0, 30.5), Vec3::new(62.0, 4.0, 1.0)),
        (Vec3::new(-30.5, 2.0, 0.0), Vec3::new(1.0, 4.0, 62.0)),
        (Vec3::new(30.5, 2.0, 0.0), Vec3::new(1.0, 4.0, 62.0)),
    ] {
        boxes.push(LevelBox { center, size, color: wall });
    }

    // Cover and platforms (a 1 m step is jumpable, 2 m is not)
    for (center, size) in [
        (Vec3::new(-6.0, 1.0, -4.0), Vec3::new(2.0, 2.0, 2.0)),
        (Vec3::new(0.0, 1.0, -8.0), Vec3::new(4.0, 2.0, 2.0)),
        (Vec3::new(6.0, 1.0, -4.0), Vec3::new(2.0, 2.0, 2.0)),
        (Vec3::new(10.0, 0.5, 5.0), Vec3::new(4.0, 1.0, 4.0)),
        (Vec3::new(-8.0, 0.5, 8.0), Vec3::new(3.0, 1.0, 3.0)),
        // Long low walls that split the middle into lanes.
        (Vec3::new(-14.0, 1.0, -16.0), Vec3::new(10.0, 2.0, 1.0)),
        (Vec3::new(14.0, 1.0, 16.0), Vec3::new(10.0, 2.0, 1.0)),
        (Vec3::new(0.0, 1.0, 14.0), Vec3::new(1.0, 2.0, 8.0)),
    ] {
        boxes.push(LevelBox { center, size, color: cover });
    }

    // Two sniper towers, each a 4 m high platform with a staircase of 1 m steps (all jumpable).
    let tower = Color::srgb(0.35, 0.38, 0.5);
    let steps = Color::srgb(0.45, 0.48, 0.6);
    for side in [-1.0_f32, 1.0] {
        boxes.push(LevelBox {
            center: Vec3::new(27.0 * side, 2.0, 0.0),
            size: Vec3::new(6.0, 4.0, 8.0),
            color: tower,
        });
        for (step, height) in [(0, 3.0_f32), (1, 2.0), (2, 1.0)] {
            boxes.push(LevelBox {
                center: Vec3::new((23.0 - step as f32 * 2.0) * side, height / 2.0, 0.0),
                size: Vec3::new(2.0, height, 4.0),
                color: steps,
            });
        }
    }

    boxes
}

/// Collision boxes as `(center, half_extents)`.
#[derive(Resource)]
pub struct LevelGeometry(pub Vec<(Vec3, Vec3)>);

pub struct LevelPlugin;

impl Plugin for LevelPlugin {
    fn build(&self, app: &mut App) {
        let solids = level_boxes()
            .iter()
            .map(|b| (b.center, b.size / 2.0))
            .collect();
        app.insert_resource(LevelGeometry(solids));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_points_are_in_the_open() {
        for spawn in spawn_points() {
            // The player's body is about 0.4 m either side and 1.8 m tall, from the floor.
            let body_center = Vec3::new(spawn.x, 0.9, spawn.z);
            for b in level_boxes() {
                let overlap = (body_center - b.center).abs().cmple(Vec3::new(0.4, 0.9, 0.4) + b.size / 2.0);
                assert!(!overlap.all(), "spawn {spawn} is inside a level box");
            }
        }
    }
}
