//! The game client: connects to a server, predicts its own movement, renders replicated players.
//!
//! Client-side prediction in one paragraph: every tick we number our input, apply it to our
//! own predicted state immediately (so movement feels instant), and send it. The server's
//! state for us arrives some time later, already containing inputs up to `last_seq`. We take
//! that state, throw away the inputs it already includes, and replay the remaining ones on
//! top. If the server agreed with our prediction the result is identical; if not, we get
//! corrected without ever waiting on the network.
//!
//! Shooting is *requested* by the client (`fire` in the input) but *decided* by the server.
//! The client never says "I hit player X"; it only says "I pulled the trigger".

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::SystemTime;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use bevy_replicon::{client::confirm_history::ConfirmHistory, prelude::*};
use bevy_replicon_renet::{
    RenetChannelsExt, RenetClient,
    netcode::{ClientAuthentication, NetcodeClientTransport},
    renet::ConnectionConfig,
};

use crate::combat::{MAX_HEALTH, Target, aim_direction, trace};
use crate::model::{SoldierAssets, WalkCycle, animate_soldiers, create_assets, spawn_soldier};
use crate::player::{EYE_HEIGHT, eye_height, step_player};
use crate::protocol::{
    HealthUpdate, HitFeedback, InputFrame, PROTOCOL_ID, PlayerInput, PlayerOwner, PlayerState,
    ShotFx,
};
use crate::menu::{AppScreen, Link, LinkPhase, MenuPlugin, MenuState, Notice};
use crate::world::{LevelGeometry, level_boxes};

const MOUSE_SENSITIVITY: f32 = 0.002;
/// How many of the latest input frames go into every packet (redundancy against packet loss).
const FRAMES_PER_PACKET: usize = 4;
/// Inputs the server hasn't confirmed yet. Bounded so a dead connection can't grow it forever.
const MAX_HISTORY: usize = 128;
/// How fast other players' bodies catch up to their replicated position (higher = snappier).
const REMOTE_SMOOTHING: f32 = 25.0;
/// How fast the camera moves to a new eye height when crouching or standing up.
const EYE_HEIGHT_SMOOTHING: f32 = 15.0;
/// Remote bodies lag their replicated position by about this many ticks because of smoothing.
/// Subtracted from the tick we report so the server rewinds to what we actually saw.
const RENDER_DELAY_TICKS: u32 = 3;

const FOV_NORMAL: f32 = 70.0_f32.to_radians();
const FOV_SCOPED: f32 = 12.0_f32.to_radians();
/// Mouse sensitivity multiplier while scoped (so aiming isn't twitchy at high zoom).
const SCOPED_SENSITIVITY: f32 = 0.25;
const HIT_MARKER_SECONDS: f32 = 0.25;
const TRACER_SECONDS: f32 = 0.35;

/// Set when the game was started with `client <ip> [port]`: connect straight away, skip the list.
#[derive(Resource)]
struct DirectTarget(Option<SocketAddr>);

/// Our own netcode client id, used to recognise which replicated player is "me".
#[derive(Resource)]
struct LocalClientId(u64);

/// Where this client is looking. Applied instantly for responsiveness and sent to the server.
#[derive(Resource, Default)]
struct LocalLook {
    yaw: f32,
    pitch: f32,
}

/// Our own predicted player.
#[derive(Resource, Default)]
struct Prediction {
    /// Current predicted state. `None` until the first server state for us arrives.
    state: Option<PlayerState>,
    /// Feet position one tick ago, used to smooth rendering between ticks.
    prev_feet: Vec3,
    /// Inputs sent but not yet confirmed by the server, oldest first.
    history: VecDeque<InputFrame>,
    next_seq: u32,
}

/// The camera's eye height above the feet, eased so crouching doesn't teleport the view.
#[derive(Resource)]
struct SmoothedEyeHeight(f32);

impl Default for SmoothedEyeHeight {
    fn default() -> Self {
        Self(EYE_HEIGHT)
    }
}

/// Trigger pulls waiting for the next fixed tick (a click lasts one frame, a tick may not run).
#[derive(Resource, Default)]
struct FireQueued(bool);

#[derive(Resource, Default)]
struct Scoped(bool);

/// Our health, as told to us by the server. Only we ever receive this.
#[derive(Resource)]
struct LocalHealth(f32);

impl Default for LocalHealth {
    fn default() -> Self {
        Self(MAX_HEALTH)
    }
}

#[derive(Resource, Default)]
struct HitMarker {
    timer: f32,
    headshot: bool,
    kill: bool,
}

#[derive(Resource, Default)]
struct Tracers(Vec<(Vec3, Vec3, f32)>);

/// The one camera we render through.
#[derive(Component)]
struct ViewCamera;

/// Marks a replicated player entity we've already set up visuals for.
#[derive(Component)]
struct PlayerVisual;

/// The player entity that belongs to this client.
#[derive(Component)]
struct LocalPlayer;

#[derive(Component)]
struct HealthText;
#[derive(Component)]
struct DeathText;
#[derive(Component)]
struct HitMarkerText;
#[derive(Component)]
struct ScopeOverlay;
#[derive(Component)]
struct Crosshair;
#[derive(Component)]
struct PauseOverlay;
#[derive(Component)]
struct DisconnectButton;

pub struct GameClientPlugin {
    /// `Some` to connect immediately on start; `None` to show the main menu.
    pub direct: Option<SocketAddr>,
}

impl Plugin for GameClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MenuPlugin)
        .insert_resource(DirectTarget(self.direct))
        .init_resource::<LocalLook>()
        .init_resource::<Prediction>()
        .init_resource::<FireQueued>()
        .init_resource::<SmoothedEyeHeight>()
        .init_resource::<Scoped>()
        .init_resource::<LocalHealth>()
        .init_resource::<HitMarker>()
        .init_resource::<Tracers>()
        .add_systems(
            Startup,
            (auto_connect, spawn_level_visuals, create_assets),
        )
        .add_systems(
            OnEnter(AppScreen::Playing),
            (spawn_hud, spawn_pause_overlay),
        )
        .add_systems(OnEnter(AppScreen::Menu), reset_session)
        .add_systems(Update, track_connection)
        .add_systems(
            Update,
            (pause_overlay, disconnect_button).run_if(in_state(AppScreen::Playing)),
        )
        .add_systems(
            Update,
            (
                // Must run before `grab_cursor`, so the click that captures the mouse doesn't fire.
                read_fire_and_scope,
                grab_cursor,
                look,
                setup_players,
                reconcile,
                sync_remote_players,
                animate_soldiers,
                update_camera,
                receive_server_messages,
                update_hud,
                draw_tracers,
            )
                .chain()
                .run_if(in_state(AppScreen::Playing)),
        )
        .add_systems(
            FixedUpdate,
            predict_and_send.run_if(in_state(ClientState::Connected)),
        );
    }
}

/// Starts a connection attempt. The result shows up later through `track_connection`.
pub fn begin_connect(
    commands: &mut Commands,
    channels: &RepliconChannels,
    link: &mut Link,
    server_addr: SocketAddr,
) -> Result<()> {
    info!("connecting to {server_addr}");

    let client = RenetClient::new(ConnectionConfig {
        server_channels_config: channels.server_configs(),
        client_channels_config: channels.client_configs(),
        ..Default::default()
    });

    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?;
    let client_id = current_time.as_millis() as u64;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    let authentication = ClientAuthentication::Unsecure {
        client_id,
        protocol_id: PROTOCOL_ID,
        server_addr,
        user_data: None,
    };
    let transport = NetcodeClientTransport::new(current_time, authentication, socket)?;

    commands.insert_resource(client);
    commands.insert_resource(transport);
    commands.insert_resource(LocalClientId(client_id));

    link.target = Some(server_addr);
    link.phase = LinkPhase::Connecting;
    link.user_requested = false;
    Ok(())
}

/// `client <ip> [port]` on the command line: connect immediately.
fn auto_connect(
    mut commands: Commands,
    channels: Res<RepliconChannels>,
    target: Res<DirectTarget>,
    mut link: ResMut<Link>,
    mut menu: ResMut<MenuState>,
) {
    let Some(addr) = target.0 else {
        return;
    };
    if let Err(error) = begin_connect(&mut commands, &channels, &mut link, addr) {
        menu.status = Some(Notice {
            text: format!("Could not start the connection: {error}"),
            error: true,
        });
    }
}

/// Watches the connection and moves between the menu and the game:
/// connected -> play; failed, lost or cancelled -> back to the menu with an explanation.
fn track_connection(
    mut commands: Commands,
    mut link: ResMut<Link>,
    mut menu: ResMut<MenuState>,
    client_state: Res<State<ClientState>>,
    mut next_screen: ResMut<NextState<AppScreen>>,
    client: Option<Res<RenetClient>>,
    transport: Option<Res<NetcodeClientTransport>>,
) {
    let target = link.target.map(|a| a.to_string()).unwrap_or_default();
    let reason = transport.as_ref().and_then(|t| t.disconnect_reason());
    let gone = client.is_none() || transport.is_none();

    let phase = link.phase;
    let notice = match phase {
        LinkPhase::Idle => return,
        LinkPhase::Connecting => {
            if *client_state.get() == ClientState::Connected {
                link.phase = LinkPhase::Connected;
                next_screen.set(AppScreen::Playing);
                return;
            }
            if !gone && reason.is_none() {
                return;
            }
            if link.user_requested {
                None
            } else {
                Some(format!(
                    "Could not connect to {target} ({}). Is the server running and its UDP port open?",
                    reason.map_or("no response".to_string(), |r| format!("{r:?}"))
                ))
            }
        }
        LinkPhase::Connected => {
            let dropped = client.as_ref().is_some_and(|c| c.is_disconnected());
            if !gone && reason.is_none() && !dropped {
                return;
            }
            next_screen.set(AppScreen::Menu);
            if link.user_requested {
                None
            } else {
                Some(format!(
                    "Disconnected from {target} ({}).",
                    reason.map_or("connection lost".to_string(), |r| format!("{r:?}"))
                ))
            }
        }
    };

    commands.remove_resource::<RenetClient>();
    commands.remove_resource::<NetcodeClientTransport>();
    commands.remove_resource::<LocalClientId>();
    link.phase = LinkPhase::Idle;
    link.user_requested = false;
    menu.status = notice.map(|text| Notice { text, error: true });
    menu.dirty = true;
}

/// Back at the menu: throw away everything from the last session.
fn reset_session(
    mut commands: Commands,
    players: Query<Entity, With<PlayerOwner>>,
    hud: Query<
        Entity,
        Or<(
            With<HealthText>,
            With<DeathText>,
            With<HitMarkerText>,
            With<ScopeOverlay>,
            With<Crosshair>,
            With<PauseOverlay>,
        )>,
    >,
    mut look: ResMut<LocalLook>,
    mut prediction: ResMut<Prediction>,
    mut fire: ResMut<FireQueued>,
    mut scoped: ResMut<Scoped>,
    mut health: ResMut<LocalHealth>,
    mut hit_marker: ResMut<HitMarker>,
    mut tracers: ResMut<Tracers>,
    mut eye: ResMut<SmoothedEyeHeight>,
    camera: Single<(&mut Transform, &mut Projection), With<ViewCamera>>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
) {
    for entity in players.iter().chain(hud.iter()) {
        commands.entity(entity).despawn();
    }

    *look = LocalLook::default();
    *prediction = Prediction::default();
    *fire = FireQueued::default();
    *scoped = Scoped::default();
    *health = LocalHealth::default();
    *hit_marker = HitMarker::default();
    *tracers = Tracers::default();
    *eye = SmoothedEyeHeight::default();

    // The menu backdrop: the static overview camera from startup.
    let (mut transform, mut projection) = camera.into_inner();
    *transform = Transform::from_xyz(0.0, 8.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y);
    if let Projection::Perspective(perspective) = &mut *projection {
        perspective.fov = FOV_NORMAL;
    }

    cursor.grab_mode = CursorGrabMode::None;
    cursor.visible = true;
}

/// Shown while the mouse is released: how to resume, and the way out of the game.
fn spawn_pause_overlay(mut commands: Commands) {
    commands.spawn((
        PauseOverlay,
        Visibility::Visible,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(58.0),
            width: Val::Px(280.0),
            margin: UiRect::left(Val::Px(-140.0)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(10.0),
            ..default()
        },
        children![
            (
                Text::new("PAUSED"),
                TextFont {
                    font_size: 36.0,
                    ..default()
                },
                TextColor(Color::WHITE),
            ),
            (
                Text::new("Click to resume"),
                TextFont {
                    font_size: 22.0,
                    ..default()
                },
                TextColor(Color::srgb(0.8, 0.8, 0.8)),
            ),
            (
                DisconnectButton,
                Button,
                Node {
                    width: Val::Px(180.0),
                    height: Val::Px(40.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(Color::srgb(0.55, 0.18, 0.18)),
                children![(
                    Text::new("Disconnect"),
                    TextFont {
                        font_size: 20.0,
                        ..default()
                    },
                    TextColor(Color::WHITE),
                )],
            ),
        ],
    ));
}

fn pause_overlay(
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut overlay: Single<&mut Visibility, With<PauseOverlay>>,
) {
    **overlay = if cursor.grab_mode == CursorGrabMode::Locked {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
}

fn disconnect_button(
    mut commands: Commands,
    buttons: Query<&Interaction, (Changed<Interaction>, With<DisconnectButton>)>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut link: ResMut<Link>,
    mut transport: Option<ResMut<NetcodeClientTransport>>,
) {
    if cursor.grab_mode == CursorGrabMode::Locked {
        return;
    }
    for interaction in &buttons {
        if *interaction == Interaction::Pressed {
            // Tell the server we're leaving, then drop the connection; `track_connection`
            // notices and returns us to the menu.
            link.user_requested = true;
            if let Some(transport) = transport.as_mut() {
                transport.disconnect();
            }
            commands.remove_resource::<RenetClient>();
            commands.remove_resource::<NetcodeClientTransport>();
        }
    }
}

fn spawn_level_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // The camera (positioned each frame from our predicted player).
    commands.spawn((
        ViewCamera,
        Camera3d::default(),
        Transform::from_xyz(0.0, 8.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands.spawn((
        DirectionalLight {
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(8.0, 16.0, 8.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(60.0, 60.0))),
        MeshMaterial3d(materials.add(Color::srgb(0.3, 0.5, 0.3))),
    ));

    for level_box in level_boxes() {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(
                level_box.size.x,
                level_box.size.y,
                level_box.size.z,
            ))),
            MeshMaterial3d(materials.add(level_box.color)),
            Transform::from_translation(level_box.center),
        ));
    }
}

fn spawn_hud(mut commands: Commands) {
    // Crosshair dot.
    commands.spawn((
        Crosshair,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(4.0),
            height: Val::Px(4.0),
            margin: UiRect::all(Val::Px(-2.0)),
            ..default()
        },
        BackgroundColor(Color::WHITE),
    ));

    // Scope: thin crosshair lines, only visible while scoped.
    commands.spawn((
        ScopeOverlay,
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(0.0),
            top: Val::Percent(50.0),
            width: Val::Percent(100.0),
            height: Val::Px(1.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
    ));
    commands.spawn((
        ScopeOverlay,
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(0.0),
            width: Val::Px(1.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
    ));

    // Your own health. Other players' health is never sent to you.
    commands.spawn((
        HealthText,
        Text::new("HP 100"),
        TextFont {
            font_size: 32.0,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(20.0),
            bottom: Val::Px(20.0),
            ..default()
        },
    ));

    // Hit marker next to the crosshair.
    commands.spawn((
        HitMarkerText,
        Text::new("X"),
        TextFont {
            font_size: 28.0,
            ..default()
        },
        TextColor(Color::WHITE),
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            margin: UiRect {
                left: Val::Px(-8.0),
                top: Val::Px(-18.0),
                ..default()
            },
            ..default()
        },
    ));

    // Death message.
    commands.spawn((
        DeathText,
        Text::new("YOU DIED - respawning..."),
        TextFont {
            font_size: 48.0,
            ..default()
        },
        TextColor(Color::srgb(0.9, 0.1, 0.1)),
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(30.0),
            margin: UiRect::left(Val::Px(-230.0)),
            ..default()
        },
    ));
}

/// Left click = fire, hold right click = scope. Only counts while the mouse is captured, and
/// the click that captures the mouse is ignored (this system runs before `grab_cursor`).
fn read_fire_and_scope(
    mouse: Res<ButtonInput<MouseButton>>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut fire: ResMut<FireQueued>,
    mut scoped: ResMut<Scoped>,
) {
    let captured = cursor.grab_mode == CursorGrabMode::Locked;
    if captured && mouse.just_pressed(MouseButton::Left) {
        fire.0 = true;
    }
    scoped.0 = captured && mouse.pressed(MouseButton::Right);
}

/// Click to capture the mouse, Escape to release it.
fn grab_cursor(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    pause_buttons: Query<&Interaction, With<DisconnectButton>>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
) {
    // Clicking the Disconnect button must not recapture the mouse.
    let over_button = pause_buttons.iter().any(|i| *i != Interaction::None);
    if mouse.just_pressed(MouseButton::Left) && !over_button {
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    }
    if keys.just_pressed(KeyCode::Escape) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

fn look(
    motion: Res<AccumulatedMouseMotion>,
    scoped: Res<Scoped>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut look: ResMut<LocalLook>,
) {
    if cursor.grab_mode != CursorGrabMode::Locked {
        return;
    }
    let sensitivity = MOUSE_SENSITIVITY * if scoped.0 { SCOPED_SENSITIVITY } else { 1.0 };
    look.yaw -= motion.delta.x * sensitivity;
    look.pitch = (look.pitch - motion.delta.y * sensitivity).clamp(-1.54, 1.54);
}

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

/// Every fixed tick: record an input, apply it to our predicted player right away,
/// and send the latest few inputs to the server.
fn predict_and_send(
    keys: Res<ButtonInput<KeyCode>>,
    look: Res<LocalLook>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    level: Res<LevelGeometry>,
    mut fire_queued: ResMut<FireQueued>,
    mut prediction: ResMut<Prediction>,
    histories: Query<&ConfirmHistory, Without<LocalPlayer>>,
    remote_players: Query<
        (Entity, &PlayerState, &Transform),
        (With<PlayerVisual>, Without<LocalPlayer>),
    >,
    mut tracers: ResMut<Tracers>,
    mut hit_marker: ResMut<HitMarker>,
    mut writer: MessageWriter<PlayerInput>,
) {
    let active = cursor.grab_mode == CursorGrabMode::Locked;
    let axis = |positive: KeyCode, negative: KeyCode| -> f32 {
        if !active {
            return 0.0;
        }
        keys.pressed(positive) as i8 as f32 - keys.pressed(negative) as i8 as f32
    };

    let prediction = &mut *prediction;

    prediction.next_seq += 1;
    let frame = InputFrame {
        seq: prediction.next_seq,
        forward: axis(KeyCode::KeyW, KeyCode::KeyS),
        right: axis(KeyCode::KeyD, KeyCode::KeyA),
        jump: active && keys.pressed(KeyCode::Space),
        crouch: active && (keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::KeyC)),
        yaw: look.yaw,
        pitch: look.pitch,
        fire: std::mem::take(&mut fire_queued.0) && active,
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
            predict_shot(&state, &level.0, &remote_players, &mut tracers, &mut hit_marker);
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
/// This is only *cosmetic*: a tracer and a hit marker. We run the same hit test the server will
/// run, against the other players as they're drawn on our screen (which is what the server
/// rewinds to). Damage, kills and health are never predicted; the server confirms those.
fn predict_shot(
    state: &PlayerState,
    walls: &[(Vec3, Vec3)],
    remote_players: &Query<
        (Entity, &PlayerState, &Transform),
        (With<PlayerVisual>, Without<LocalPlayer>),
    >,
    tracers: &mut Tracers,
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

    tracers.0.push((
        state.position,
        state.position + direction * result.end_distance,
        TRACER_SECONDS,
    ));

    if let Some(hit) = result.hit {
        hit_marker.timer = HIT_MARKER_SECONDS;
        hit_marker.headshot = hit.headshot;
        // We can't know it's a kill: that depends on health the server never tells us.
        hit_marker.kill = false;
    }
}

/// Newly replicated players: mark which one is ours, and give the others a body.
fn setup_players(
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
            ));
            let parts = spawn_soldier(&mut commands, entity, &assets);
            commands.entity(entity).insert(parts);
        }
    }
}

/// Reconciliation: start from the server's latest state for us, drop the inputs it has already
/// applied, and replay the rest on top. Run every frame; if the server agreed with our
/// prediction the result doesn't change at all.
fn reconcile(
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
fn sync_remote_players(
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
fn update_camera(
    time: Res<Time>,
    look: Res<LocalLook>,
    scoped: Res<Scoped>,
    prediction: Res<Prediction>,
    fixed: Res<Time<Fixed>>,
    mut eye: ResMut<SmoothedEyeHeight>,
    camera: Single<(&mut Transform, &mut Projection), With<ViewCamera>>,
) {
    let (mut transform, mut projection) = camera.into_inner();

    if let Projection::Perspective(perspective) = &mut *projection {
        let target = if scoped.0 { FOV_SCOPED } else { FOV_NORMAL };
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
    transform.translation = prediction
        .prev_feet
        .lerp(feet, fixed.overstep_fraction())
        + Vec3::Y * eye.0;
    transform.rotation = Quat::from_euler(EulerRot::YXZ, look.yaw, look.pitch, 0.0);
}

/// Messages from the server: our health, hit confirmations, and everyone's shot tracers.
fn receive_server_messages(
    mut health: ResMut<LocalHealth>,
    mut hit_marker: ResMut<HitMarker>,
    mut tracers: ResMut<Tracers>,
    mut health_in: MessageReader<HealthUpdate>,
    mut hits_in: MessageReader<HitFeedback>,
    mut shots_in: MessageReader<ShotFx>,
) {
    for update in health_in.read() {
        health.0 = update.health;
    }
    for hit in hits_in.read() {
        hit_marker.timer = HIT_MARKER_SECONDS;
        hit_marker.headshot = hit.headshot;
        hit_marker.kill = hit.kill;
    }
    for shot in shots_in.read() {
        tracers.0.push((shot.origin, shot.end, TRACER_SECONDS));
    }
}

fn update_hud(
    time: Res<Time>,
    health: Res<LocalHealth>,
    scoped: Res<Scoped>,
    prediction: Res<Prediction>,
    mut hit_marker: ResMut<HitMarker>,
    mut health_text: Single<(&mut Text, &mut TextColor), With<HealthText>>,
    mut death_text: Single<&mut Visibility, (With<DeathText>, Without<HitMarkerText>)>,
    mut marker: Single<
        (&mut Visibility, &mut TextColor),
        (With<HitMarkerText>, Without<DeathText>, Without<HealthText>),
    >,
    mut scope_lines: Query<
        &mut Visibility,
        (With<ScopeOverlay>, Without<DeathText>, Without<HitMarkerText>),
    >,
) {
    let (text, color) = &mut *health_text;
    text.0 = format!("HP {}", health.0.ceil() as i32);
    color.0 = if health.0 <= 30.0 {
        Color::srgb(0.9, 0.2, 0.2)
    } else {
        Color::WHITE
    };

    let alive = prediction.state.is_none_or(|s| s.alive);
    **death_text = if alive {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };

    hit_marker.timer = (hit_marker.timer - time.delta_secs()).max(0.0);
    let (marker_visibility, marker_color) = &mut *marker;
    **marker_visibility = if hit_marker.timer > 0.0 {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    marker_color.0 = if hit_marker.kill {
        Color::srgb(1.0, 0.1, 0.1)
    } else if hit_marker.headshot {
        Color::srgb(1.0, 0.85, 0.1)
    } else {
        Color::WHITE
    };

    for mut visibility in &mut scope_lines {
        *visibility = if scoped.0 && alive {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

fn draw_tracers(time: Res<Time>, mut tracers: ResMut<Tracers>, mut gizmos: Gizmos) {
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
