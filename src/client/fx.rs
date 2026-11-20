//! Visual effects for shots: tracers, muzzle flashes, impact particles and bullet marks.
//! Purely cosmetic and client-side.

use std::collections::VecDeque;

use super::*;
use crate::combat::RANGE;

const TRACER_SECONDS: f32 = 0.35;
const FLASH_SECONDS: f32 = 0.06;
const PARTICLE_SECONDS: f32 = 0.55;
const PARTICLE_GRAVITY: f32 = 9.0;
/// Bullet marks left on walls. Older ones are removed first.
const MAX_BULLET_MARKS: usize = 48;

#[derive(Resource)]
pub(super) struct FxAssets {
    cube: Handle<Mesh>,
    dust: Handle<StandardMaterial>,
    blood: Handle<StandardMaterial>,
    mark: Handle<StandardMaterial>,
}

#[derive(Resource, Default)]
pub(super) struct Tracers(Vec<(Vec3, Vec3, f32)>);

/// Bullet marks in the order they were made.
#[derive(Resource, Default)]
pub(super) struct BulletMarks(VecDeque<Entity>);

#[derive(Component)]
pub(super) struct Particle {
    velocity: Vec3,
    ttl: f32,
}

#[derive(Component)]
pub(super) struct MuzzleFlash(f32);

#[derive(Component)]
pub(super) struct FxEntity;

pub(super) fn setup_fx(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let flat = |color: Color| StandardMaterial {
        base_color: color,
        unlit: true,
        ..default()
    };
    commands.insert_resource(FxAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        dust: materials.add(flat(Color::srgb(0.75, 0.72, 0.62))),
        blood: materials.add(flat(Color::srgb(0.8, 0.05, 0.05))),
        mark: materials.add(flat(Color::srgb(0.06, 0.06, 0.06))),
    });
    commands.init_resource::<BulletMarks>();
}

pub(super) fn reset_fx(
    mut commands: Commands,
    effects: Query<Entity, With<FxEntity>>,
    mut tracers: ResMut<Tracers>,
    marks: Option<ResMut<BulletMarks>>,
) {
    for entity in &effects {
        commands.entity(entity).despawn();
    }
    *tracers = Tracers::default();
    if let Some(mut marks) = marks {
        marks.0.clear();
    }
}

/// A cheap deterministic pseudo-random number in `-1.0..=1.0`.
fn noise(seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*seed >> 8) as f32 / (1 << 23) as f32 - 1.0
}

/// Every shot gets a tracer and a muzzle flash; shots that hit something get an impact.
pub(super) fn spawn_shot_effects(
    mut commands: Commands,
    mut shots: MessageReader<ShotFired>,
    mut tracers: ResMut<Tracers>,
    mut marks: ResMut<BulletMarks>,
    assets: Res<FxAssets>,
    mut seed: Local<u32>,
) {
    for shot in shots.read() {
        tracers.0.push((shot.origin, shot.end, TRACER_SECONDS));

        let length = shot.origin.distance(shot.end);
        let direction = (shot.end - shot.origin).normalize_or_zero();

        commands.spawn((
            FxEntity,
            MuzzleFlash(FLASH_SECONDS),
            PointLight {
                intensity: 500_000.0,
                range: 14.0,
                color: Color::srgb(1.0, 0.8, 0.45),
                shadows_enabled: false,
                ..default()
            },
            Transform::from_translation(shot.origin + direction * 1.2),
        ));

        if length >= RANGE - 1.0 {
            continue;
        }

        let (material, count) = if shot.hit_player {
            (assets.blood.clone(), 14)
        } else {
            (assets.dust.clone(), 9)
        };
        for _ in 0..count {
            let scatter = Vec3::new(noise(&mut seed), noise(&mut seed), noise(&mut seed));
            commands.spawn((
                FxEntity,
                Particle {
                    velocity: -direction * 2.0 + scatter * 2.5 + Vec3::Y * 1.5,
                    ttl: PARTICLE_SECONDS,
                },
                Mesh3d(assets.cube.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(shot.end).with_scale(Vec3::splat(0.07)),
            ));
        }

        if !shot.hit_player {
            let mark = commands
                .spawn((
                    FxEntity,
                    Mesh3d(assets.cube.clone()),
                    MeshMaterial3d(assets.mark.clone()),
                    Transform::from_translation(shot.end - direction * 0.01)
                        .with_scale(Vec3::splat(0.14)),
                ))
                .id();
            marks.0.push_back(mark);
            while marks.0.len() > MAX_BULLET_MARKS {
                if let Some(old) = marks.0.pop_front() {
                    commands.entity(old).despawn();
                }
            }
        }
    }
}

pub(super) fn update_effects(
    time: Res<Time>,
    mut commands: Commands,
    mut particles: Query<(Entity, &mut Transform, &mut Particle)>,
    mut flashes: Query<(Entity, &mut MuzzleFlash)>,
) {
    let dt = time.delta_secs();
    for (entity, mut transform, mut particle) in &mut particles {
        particle.ttl -= dt;
        if particle.ttl <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        particle.velocity.y -= PARTICLE_GRAVITY * dt;
        transform.translation += particle.velocity * dt;
        transform.scale = Vec3::splat(0.07 * (particle.ttl / PARTICLE_SECONDS).max(0.1));
    }
    for (entity, mut flash) in &mut flashes {
        flash.0 -= dt;
        if flash.0 <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

pub(super) fn draw_tracers(time: Res<Time>, mut tracers: ResMut<Tracers>, mut gizmos: Gizmos) {
    let dt = time.delta_secs();
    tracers.0.retain_mut(|(start, end, ttl)| {
        *ttl -= dt;
        if *ttl > 0.0 {
            // Start slightly ahead of the eye so our own tracer isn't a line through the camera.
            let from = start.lerp(*end, (0.5 / start.distance(*end).max(0.5)).min(1.0));
            gizmos.line(from, *end, Color::srgb(1.0, 0.9, 0.4));
            true
        } else {
            false
        }
    });
}
