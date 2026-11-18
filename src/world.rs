//! The level layout, shared by client and server.
//!
//! The server only needs the collision boxes; the client also builds visuals from the
//! same list, so what you see always matches what you collide with.

use bevy::prelude::*;

use crate::player::EYE_HEIGHT;

/// Where players (re)spawn, as eye positions.
pub fn spawn_points() -> [Vec3; 4] {
    [
        Vec3::new(0.0, EYE_HEIGHT, 25.0),
        Vec3::new(-24.0, EYE_HEIGHT, -24.0),
        Vec3::new(24.0, EYE_HEIGHT, -24.0),
        Vec3::new(-24.0, EYE_HEIGHT, 24.0),
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
    ] {
        boxes.push(LevelBox { center, size, color: cover });
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
