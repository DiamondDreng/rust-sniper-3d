//! Prediction and reconciliation of our own player, the camera, and the bodies of other players.

use bevy_replicon::client::confirm_history::ConfirmHistory;

use super::*;
use crate::combat::{Target, aim_direction, trace};
use crate::model::{SoldierAssets, WalkCycle, spawn_soldier};
use crate::player::{eye_height, step_player};
use crate::protocol::{PlayerInput, PlayerName, SetName, sanitize_name};
use crate::settings::Action;
use crate::world::LevelGeometry;

type RemotePlayers<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static PlayerState, &'static Transform),
    (With<PlayerVisual>, Without<LocalPlayer>),
>;

/// The newest server tick we have received data for, minus our render delay.
/// This is the moment in time the other players on our screen correspond to.
/// Returns `u32::MAX` if we don't know yet (the server clamps it to "now", i.e. no rewind).
fn viewed_server_tick(histories: &Query<&ConfirmHistory, Without<LocalPlayer>>) -> u32 {
    let mut newest: Option<RepliconTick> = None;
    for history in histories {
        let tick = history.last_tick();
        if newest.is_none_or(|best| tick > best) {
            newest = Some(tick);
        }
    }
    match newest {
        Some(tick) => tick.get().saturating_sub(RENDER_DELAY_TICKS),
        None => u32::MAX,
    }
}

/// Where we aim: the mouse look plus scope sway.
fn aim(look: &LocalLook, sway: &ScopeSway) -> (f32, f32) {
    (
        look.yaw + sway.yaw,
        (look.pitch + sway.pitch).clamp(-1.54, 1.54),
    )
}

/// Every fixed tick: record an input, apply it to our predicted player right away,
/// and send the latest few inputs to the server.
#[allow(clippy::too_many_arguments)]
pub(super) fn predict_and_send(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<Settings>,
    look: Res<LocalLook>,
    sway: Res<ScopeSway>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    level: Res<LevelGeometry>,
    mut fire_queued: ResMut<FireQueued>,
    mut prediction: ResMut<Prediction>,
    histories: Query<&ConfirmHistory, Without<LocalPlayer>>,
    remote_players: RemotePlayers,
    mut shots: MessageWriter<ShotFired>,
    mut hit_marker: ResMut<HitMarker>,
    mut writer: MessageWriter<PlayerInput>,
) {
    let active = cursor.grab_mode == CursorGrabMode::Locked;
    let down = |action: Action| active && keys.pressed(settings.key(action));
    let axis = |positive: Action, negative: Action| -> f32 {
        down(positive) as i8 as f32 - down(negative) as i8 as f32
    };

    let prediction = &mut *prediction;
    let (yaw, pitch) = aim(&look, &sway);

    prediction.next_seq += 1;
    let frame = InputFrame {
        seq: prediction.next_seq,
        forward: axis(Action::Forward, Action::Back),
        right: axis(Action::Right, Action::Left),
        jump: down(Action::Jump),
        crouch: down(Action::Crouch),
        yaw,
        pitch,
        fire: std::mem::take(&mut fire_queued.0) && active,
        reload: down(Action::Reload),
        view_tick: viewed_server_tick(&histories),
    };

    prediction.history.push_back(frame);
    while prediction.history.len() > MAX_HISTORY {
        prediction.history.pop_front();
    }

    // Predict: apply the input to ourselves immediately.
    if let Some(mut state) = prediction.state {
        prediction.prev_feet = state.position - Vec3::Y * eye_height(state.crouched);
        let fired = step_player(&mut state, &frame, &level.0);
        prediction.state = Some(state);

        if fired {
            predict_shot(&state, &level.0, &remote_players, &mut shots, &mut hit_marker);
        }
    }

    // Send the newest few frames; the server ignores the ones it already applied.
    let skip = prediction.history.len().saturating_sub(FRAMES_PER_PACKET);
    writer.write(PlayerInput {
        frames: prediction.history.iter().skip(skip).copied().collect(),
    });
}

/// Shows the result of our own shot immediately, before the server has answered.
///
/// This is only *cosmetic*: a tracer, impact and a hit marker. We run the same hit test the
/// server will run, against the other players as they're drawn on our screen (which is what the
/// server rewinds to). Damage, kills and health are never predicted; the server confirms those.
fn predict_shot(
    state: &PlayerState,
    walls: &[(Vec3, Vec3)],
    remote_players: &RemotePlayers,
    shots: &mut MessageWriter<ShotFired>,
    hit_marker: &mut HitMarker,
) {
    let targets: Vec<Target> = remote_players
        .iter()
        .filter(|(_, remote, _)| remote.alive)
        .map(|(entity, remote, transform)| Target {
            id: entity,
            // The body's transform is at its feet, which is what the hit test wants.
            feet: transform.translation,
            crouched: remote.crouched,
        })
        .collect();

    let direction = aim_direction(state.yaw, state.pitch);
    let result = trace(state.position, direction, walls, &targets);

    shots.write(ShotFired {
        origin: state.position,
        end: state.position + direction * result.end_distance,
        local: true,
        hit_player: result.hit.is_some(),
    });

    if let Some(hit) = result.hit {
        hit_marker.timer = HIT_MARKER_SECONDS;
        hit_marker.headshot = hit.headshot;
        // We can't know it's a kill: that depends on health the server never tells us.
        hit_marker.kill = false;
    }
}

/// Newly replicated players: mark which one is ours, and give the others a body and a name tag.
pub(super) fn setup_players(
    mut commands: Commands,
    local_id: Option<Res<LocalClientId>>,
    new_players: Query<(Entity, &PlayerOwner), Without<PlayerVisual>>,
    assets: Res<SoldierAssets>,
) {
    let Some(local_id) = local_id else {
        return;
    };
    for (entity, owner) in &new_players {
        if owner.0 == local_id.0 {
            // Our own player has no body: we look out of it.
            commands.entity(entity).insert((PlayerVisual, LocalPlayer));
        } else {
            // The entity's transform sits at the player's feet; the soldier's parts hang from it.
            commands.entity(entity).insert((
                PlayerVisual,
                Transform::default(),
                Visibility::default(),
                WalkCycle::default(),
                audio::StepTimer::default(),
            ));
            let parts = spawn_soldier(&mut commands, entity, &assets);
            commands.entity(entity).insert(parts);
            hud::spawn_name_tag(&mut commands, entity);
        }
    }
}

/// Reconciliation: start from the server's latest state for us, drop the inputs it has already
/// applied, and replay the rest on top. Run every frame; if the server agreed with our
/// prediction the result doesn't change at all.
pub(super) fn reconcile(
    level: Res<LevelGeometry>,
    mut prediction: ResMut<Prediction>,
    local: Query<&PlayerState, With<LocalPlayer>>,
) {
    let Ok(server_state) = local.single() else {
        return;
    };
    let prediction = &mut *prediction;

    prediction
        .history
        .retain(|frame| frame.seq > server_state.last_seq);

    let mut state = *server_state;
    for frame in &prediction.history {
        step_player(&mut state, frame, &level.0);
    }

    if prediction.state.is_none() {
        prediction.prev_feet = state.position - Vec3::Y * eye_height(state.crouched);
    }
    prediction.state = Some(state);
}

/// Place other players' bodies from their replicated state, smoothed so they don't
/// visibly step at the network tick rate. Dead players are hidden.
pub(super) fn sync_remote_players(
    time: Res<Time>,
    mut players: Query<
        (&PlayerState, &mut Transform, &mut Visibility),
        (With<PlayerVisual>, Without<LocalPlayer>),
    >,
) {
    let blend = 1.0 - (-REMOTE_SMOOTHING * time.delta_secs()).exp();

    for (state, mut transform, mut visibility) in &mut players {
        *visibility = if state.alive {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };

        // The entity sits at the feet (the soldier model hangs from it).
        let target = state.position - Vec3::Y * eye_height(state.crouched);
        if transform.translation == Vec3::ZERO || transform.translation.distance(target) > 5.0 {
            // Freshly spawned or respawned: appear in place instead of sliding across the map.
            transform.translation = target;
        } else {
            transform.translation = transform.translation.lerp(target, blend);
        }
        transform.rotation = Quat::from_rotation_y(state.yaw);
    }
}

/// Camera sits at our predicted eye position, looking where we look. Scoping zooms the FOV.
pub(super) fn update_camera(
    time: Res<Time>,
    look: Res<LocalLook>,
    sway: Res<ScopeSway>,
    scoped: Res<Scoped>,
    settings: Res<Settings>,
    prediction: Res<Prediction>,
    fixed: Res<Time<Fixed>>,
    mut eye: ResMut<SmoothedEyeHeight>,
    camera: Single<(&mut Transform, &mut Projection), With<ViewCamera>>,
) {
    let (mut transform, mut projection) = camera.into_inner();

    if let Projection::Perspective(perspective) = &mut *projection {
        let target = if scoped.0 {
            FOV_SCOPED
        } else {
            settings.fov.to_radians()
        };
        let blend = 1.0 - (-20.0 * time.delta_secs()).exp();
        perspective.fov += (target - perspective.fov) * blend;
    }

    let Some(state) = prediction.state else {
        return;
    };
    // Prediction advances in fixed ticks; blend the feet position between the last two ticks so a
    // 144 Hz screen is smooth, then add an eased eye height on top (so crouching glides).
    let feet = state.position - Vec3::Y * eye_height(state.crouched);
    let target_eye = eye_height(state.crouched);
    eye.0 += (target_eye - eye.0) * (1.0 - (-EYE_HEIGHT_SMOOTHING * time.delta_secs()).exp());
    transform.translation =
        prediction.prev_feet.lerp(feet, fixed.overstep_fraction()) + Vec3::Y * eye.0;
    let (yaw, pitch) = aim(&look, &sway);
    transform.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
}

/// Tells the server our nickname, and keeps telling it (once a second) until the replicated
/// name on our player matches what we want.
pub(super) fn send_name(
    time: Res<Time>,
    settings: Res<Settings>,
    local: Query<&PlayerName, With<LocalPlayer>>,
    mut writer: MessageWriter<SetName>,
    mut cooldown: Local<f32>,
) {
    *cooldown -= time.delta_secs();
    let wanted = sanitize_name(&settings.name);
    let Ok(current) = local.single() else {
        return;
    };
    if current.0 != wanted && *cooldown <= 0.0 {
        writer.write(SetName(wanted));
        *cooldown = 1.0;
    }
}
