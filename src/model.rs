//! A low-poly soldier built from a few boxes and a sphere, drawn for *other* players.
//!
//! Everything here is cosmetic and client-only. The server never sees or needs this: hit
//! detection uses simple shapes (see `combat.rs`), and the model is built to fit inside them:
//! the head sphere sits exactly where the hitbox head is, and the body is 1.8 m tall standing
//! and 1.0 m crouched.
//!
//! The animation is derived purely from the replicated `PlayerState` (speed, grounded,
//! crouched, sliding, aim pitch, fire cooldown), so it needs no extra network data.
//!
//! All parts are positioned relative to the player's *feet*, facing -Z (the forward direction).

use std::f32::consts::TAU;

use bevy::prelude::*;

use crate::combat::FIRE_COOLDOWN_TICKS;
use crate::player::eye_height;
use crate::protocol::PlayerState;

// Standing layout (meters above the feet).
const HIP_Y: f32 = 0.8;
const TORSO_HEIGHT: f32 = 0.6;
// Crouched layout: hips and torso are lower and the legs are squashed to fit.
const CROUCH_HIP_Y: f32 = 0.11;
const CROUCH_TORSO_HEIGHT: f32 = 0.39;
const LEG_LENGTH: f32 = 0.8;

/// How fast poses blend toward their target (higher = snappier).
const POSE_SMOOTHING: f32 = 18.0;

#[derive(Resource)]
pub struct SoldierAssets {
    cube: Handle<Mesh>,
    head: Handle<Mesh>,
    uniform: Handle<StandardMaterial>,
    pants: Handle<StandardMaterial>,
    skin: Handle<StandardMaterial>,
    visor: Handle<StandardMaterial>,
    rifle: Handle<StandardMaterial>,
    scope: Handle<StandardMaterial>,
}

/// Handles to the animated parts of one soldier.
#[derive(Component)]
pub struct ModelParts {
    /// `[left, right]` leg pivots (the pivot is the hip).
    legs: [Entity; 2],
    torso: Entity,
    head: Entity,
    /// Shoulder-height pivot that the arms and rifle hang from; it rotates with aim pitch.
    aim: Entity,
}

/// Walk animation phase, advanced by how fast the player moves.
#[derive(Component, Default)]
pub struct WalkCycle {
    phase: f32,
}

/// Builds the shared meshes and materials once.
pub fn create_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(SoldierAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        head: meshes.add(Sphere::new(0.2)),
        uniform: materials.add(Color::srgb(0.30, 0.38, 0.25)),
        pants: materials.add(Color::srgb(0.20, 0.22, 0.18)),
        skin: materials.add(Color::srgb(0.85, 0.68, 0.55)),
        visor: materials.add(Color::srgb(0.08, 0.08, 0.10)),
        rifle: materials.add(Color::srgb(0.14, 0.14, 0.16)),
        scope: materials.add(Color::srgb(0.05, 0.05, 0.06)),
    });
}

/// A box of the given size, centered at `translation` (relative to its parent).
fn block(
    mesh: &Handle<Mesh>,
    material: &Handle<StandardMaterial>,
    translation: Vec3,
    size: Vec3,
) -> impl Bundle {
    (
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material.clone()),
        Transform::from_translation(translation).with_scale(size),
    )
}

/// A box stretched between two points (used for arms).
fn limb(
    mesh: &Handle<Mesh>,
    material: &Handle<StandardMaterial>,
    from: Vec3,
    to: Vec3,
    thickness: f32,
) -> impl Bundle {
    let middle = (from + to) / 2.0;
    let length = from.distance(to);
    (
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material.clone()),
        Transform::from_translation(middle)
            .looking_at(to, Vec3::Y)
            .with_scale(Vec3::new(thickness, thickness, length)),
    )
}

/// Spawns a soldier as children of `root` and returns handles to the parts that animate.
pub fn spawn_soldier(commands: &mut Commands, root: Entity, assets: &SoldierAssets) -> ModelParts {
    let mut legs = [Entity::PLACEHOLDER; 2];
    let mut torso = Entity::PLACEHOLDER;
    let mut head = Entity::PLACEHOLDER;
    let mut aim = Entity::PLACEHOLDER;

    commands.entity(root).with_children(|parent| {
        // Legs: a pivot at the hip, with the leg box hanging below it.
        for (index, side) in [-1.0_f32, 1.0].into_iter().enumerate() {
            legs[index] = parent
                .spawn((
                    Transform::from_xyz(side * 0.12, HIP_Y, 0.0),
                    Visibility::default(),
                ))
                .with_children(|leg| {
                    leg.spawn(block(
                        &assets.cube,
                        &assets.pants,
                        Vec3::new(0.0, -LEG_LENGTH / 2.0, 0.0),
                        Vec3::new(0.18, LEG_LENGTH, 0.2),
                    ));
                })
                .id();
        }

        // Torso (its transform is rewritten every frame to follow the crouch).
        torso = parent
            .spawn(block(
                &assets.cube,
                &assets.uniform,
                Vec3::new(0.0, HIP_Y + TORSO_HEIGHT / 2.0, 0.0),
                Vec3::new(0.5, TORSO_HEIGHT, 0.28),
            ))
            .id();

        // Head: a sphere whose center and radius match the head hitbox, plus a dark visor on
        // the front so you can tell which way someone is facing.
        head = parent
            .spawn((
                Transform::from_xyz(0.0, eye_height(false) - 0.1, 0.0),
                Visibility::default(),
            ))
            .with_children(|head| {
                head.spawn((
                    Mesh3d(assets.head.clone()),
                    MeshMaterial3d(assets.skin.clone()),
                    Transform::default(),
                ));
                head.spawn(block(
                    &assets.cube,
                    &assets.visor,
                    Vec3::new(0.0, 0.02, -0.19),
                    Vec3::new(0.28, 0.07, 0.05),
                ));
            })
            .id();

        // Arms + sniper rifle, hanging from a shoulder-height pivot that tilts with aim pitch.
        aim = parent
            .spawn((
                Transform::from_xyz(0.0, HIP_Y + TORSO_HEIGHT - 0.05, 0.0),
                Visibility::default(),
            ))
            .with_children(|aim| {
                // Rifle body and scope.
                aim.spawn(block(
                    &assets.cube,
                    &assets.rifle,
                    Vec3::new(0.1, -0.14, -0.55),
                    Vec3::new(0.06, 0.09, 1.1),
                ));
                aim.spawn(block(
                    &assets.cube,
                    &assets.scope,
                    Vec3::new(0.1, -0.07, -0.45),
                    Vec3::new(0.05, 0.05, 0.35),
                ));
                // Right hand on the grip, left hand on the foregrip.
                aim.spawn(limb(
                    &assets.cube,
                    &assets.uniform,
                    Vec3::new(0.3, 0.0, 0.0),
                    Vec3::new(0.1, -0.16, -0.3),
                    0.11,
                ));
                aim.spawn(limb(
                    &assets.cube,
                    &assets.uniform,
                    Vec3::new(-0.3, 0.0, 0.0),
                    Vec3::new(0.1, -0.16, -0.8),
                    0.11,
                ));
            })
            .id();
    });

    ModelParts {
        legs,
        torso,
        head,
        aim,
    }
}

/// Poses every soldier from its replicated state: walk cycle, crouch, slide, aim, bolt recoil.
pub fn animate_soldiers(
    time: Res<Time>,
    mut soldiers: Query<(&PlayerState, &ModelParts, &mut WalkCycle)>,
    mut parts: Query<&mut Transform, Without<ModelParts>>,
) {
    let dt = time.delta_secs();
    let blend = 1.0 - (-POSE_SMOOTHING * dt).exp();

    for (state, model, mut cycle) in &mut soldiers {
        let crouched = state.crouched;
        let sliding = crouched && state.slide_ticks > 0;
        let walking = state.grounded && !sliding;
        let speed = Vec2::new(state.velocity.x, state.velocity.z).length();

        if walking {
            cycle.phase = (cycle.phase + speed * dt * 3.0) % TAU;
        }

        let hip_y = if crouched { CROUCH_HIP_Y } else { HIP_Y };
        let torso_height = if crouched {
            CROUCH_TORSO_HEIGHT
        } else {
            TORSO_HEIGHT
        };

        // ---- Legs ----
        let leg_scale = if sliding {
            0.6
        } else if crouched {
            CROUCH_HIP_Y / LEG_LENGTH
        } else {
            1.0
        };
        let swing = if !walking {
            0.0
        } else if crouched {
            (speed / 2.5).clamp(0.0, 1.0) * 0.25
        } else {
            (speed / 6.0).clamp(0.0, 1.2) * 0.9
        };
        for (index, leg) in model.legs.iter().enumerate() {
            let side = if index == 0 { -1.0 } else { 1.0 };
            let angle = if sliding {
                1.2 // legs stretched out in front
            } else if !state.grounded {
                if index == 0 { 0.6 } else { -0.4 } // a loose "in the air" pose
            } else {
                // Opposite legs swing in opposite directions.
                -side * cycle.phase.sin() * swing
            };
            if let Ok(mut transform) = parts.get_mut(*leg) {
                let target = Vec3::new(side * 0.12, hip_y, 0.0);
                transform.translation = transform.translation.lerp(target, blend);
                let scale = Vec3::new(1.0, leg_scale, 1.0);
                transform.scale = transform.scale.lerp(scale, blend);
                transform.rotation = transform
                    .rotation
                    .slerp(Quat::from_rotation_x(angle), blend);
            }
        }

        // ---- Torso ----
        if let Ok(mut transform) = parts.get_mut(model.torso) {
            let target = Vec3::new(0.0, hip_y + torso_height / 2.0, 0.0);
            transform.translation = transform.translation.lerp(target, blend);
            let scale = Vec3::new(0.5, torso_height, 0.28);
            transform.scale = transform.scale.lerp(scale, blend);
        }

        // ---- Head (follows the pitch a little, so the visor tilts) ----
        if let Ok(mut transform) = parts.get_mut(model.head) {
            let target = Vec3::new(0.0, eye_height(crouched) - 0.1, 0.0);
            transform.translation = transform.translation.lerp(target, blend);
            transform.rotation = transform
                .rotation
                .slerp(Quat::from_rotation_x(state.pitch * 0.8), blend);
        }

        // ---- Arms + rifle: aim pitch, plus a kick right after a shot ----
        // The fire cooldown starts at its maximum on the shot tick and counts down, so the
        // first few ticks of it are the recoil.
        let recoil =
            ((state.cooldown as f32 - (FIRE_COOLDOWN_TICKS as f32 - 10.0)) / 10.0).clamp(0.0, 1.0);
        if let Ok(mut transform) = parts.get_mut(model.aim) {
            let target = Vec3::new(0.0, hip_y + torso_height - 0.05, recoil * 0.1);
            transform.translation = transform.translation.lerp(target, blend);
            transform.rotation = transform
                .rotation
                .slerp(Quat::from_rotation_x(state.pitch + recoil * 0.12), blend);
        }
    }
}
