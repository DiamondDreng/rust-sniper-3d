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
//!
//! This file holds the shared state and the connection handling. The rest lives in modules:
//! `input` (mouse, scope, sway), `prediction` (movement, camera, remote players), `hud`
//! (everything drawn in 2D), `fx` (tracers, impacts, flashes), `audio` and `pause`
//! (pause screen and settings).

mod audio;
mod fx;
mod hud;
mod input;
mod pause;
mod prediction;

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::SystemTime;

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use bevy_replicon::prelude::*;
use bevy_replicon_renet::{
    RenetChannelsExt, RenetClient,
    netcode::{ClientAuthentication, NetcodeClientTransport},
    renet::ConnectionConfig,
};

use crate::combat::MAX_HEALTH;
use crate::menu::{AppScreen, Link, LinkPhase, MenuPlugin, MenuState, Notice};
use crate::model::{animate_soldiers, create_assets};
use crate::player::EYE_HEIGHT;
use crate::protocol::{InputFrame, PROTOCOL_ID, PlayerOwner, PlayerState};
use crate::settings::Settings;
use crate::world::level_boxes;

/// Mouse sensitivity at a settings multiplier of 1.0 (radians per pixel).
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

const FOV_SCOPED: f32 = 12.0_f32.to_radians();
/// Mouse sensitivity multiplier while scoped (so aiming isn't twitchy at high zoom).
const SCOPED_SENSITIVITY: f32 = 0.25;
const HIT_MARKER_SECONDS: f32 = 0.25;

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

/// Scope wobble, and the breath you can hold to steady it. The offset is added to where the
/// camera points *and* to the aim we send, so the shot goes where the crosshair is.
#[derive(Resource)]
struct ScopeSway {
    time: f32,
    yaw: f32,
    pitch: f32,
    /// Breath left, 1.0 = full.
    breath: f32,
    /// Seconds left of being out of breath (worse sway, can't hold).
    winded: f32,
    holding: bool,
}

impl Default for ScopeSway {
    fn default() -> Self {
        Self {
            time: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            breath: 1.0,
            winded: 0.0,
            holding: false,
        }
    }
}

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

/// A shot was fired, ours (predicted) or someone else's (from the server). Effects and sounds
/// listen for this.
#[derive(Message, Clone, Copy)]
struct ShotFired {
    origin: Vec3,
    end: Vec3,
    local: bool,
    hit_player: bool,
}

/// The one camera we render through.
#[derive(Component)]
struct ViewCamera;

/// Marks a replicated player entity we've already set up visuals for.
#[derive(Component)]
struct PlayerVisual;

/// The player entity that belongs to this client.
#[derive(Component)]
struct LocalPlayer;

pub struct GameClientPlugin {
    /// `Some` to connect immediately on start; `None` to show the main menu.
    pub direct: Option<SocketAddr>,
}

impl Plugin for GameClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MenuPlugin)
            .insert_resource(DirectTarget(self.direct))
            .insert_resource(Settings::load())
            .init_resource::<LocalLook>()
            .init_resource::<Prediction>()
            .init_resource::<FireQueued>()
            .init_resource::<SmoothedEyeHeight>()
            .init_resource::<Scoped>()
            .init_resource::<ScopeSway>()
            .init_resource::<LocalHealth>()
            .init_resource::<HitMarker>()
            .init_resource::<hud::KillLog>()
            .init_resource::<hud::MatchBanner>()
            .init_resource::<hud::DamageFlash>()
            .init_resource::<fx::Tracers>()
            .init_resource::<pause::Rebinding>()
            .add_message::<ShotFired>()
            .add_systems(
                Startup,
                (
                    auto_connect,
                    spawn_level_visuals,
                    create_assets,
                    fx::setup_fx,
                    audio::setup_audio,
                ),
            )
            .add_systems(
                OnEnter(AppScreen::Playing),
                (hud::spawn_hud, pause::spawn_pause_overlay),
            )
            .add_systems(
                OnEnter(AppScreen::Menu),
                (reset_session, hud::reset_hud, fx::reset_fx),
            )
            .add_systems(Update, (track_connection, audio::apply_volume))
            .add_systems(
                Update,
                (
                    pause::pause_overlay,
                    pause::handle_buttons,
                    pause::capture_rebind,
                    pause::refresh_texts,
                    pause::button_colors,
                )
                    .run_if(in_state(AppScreen::Playing)),
            )
            .add_systems(
                Update,
                (
                    // Must run before `grab_cursor`, so the click that captures the mouse doesn't fire.
                    input::read_fire_and_scope,
                    input::grab_cursor,
                    input::look,
                    prediction::setup_players,
                    prediction::reconcile,
                    prediction::sync_remote_players,
                    animate_soldiers,
                    input::update_sway,
                    prediction::update_camera,
                    hud::receive_server_messages,
                    prediction::send_name,
                )
                    .chain()
                    .run_if(in_state(AppScreen::Playing)),
            )
            .add_systems(
                Update,
                (
                    hud::update_health_text,
                    hud::update_ammo_text,
                    hud::update_death_text,
                    hud::update_hit_marker,
                    hud::update_scope,
                    hud::update_crosshair,
                    hud::update_damage_edges,
                    hud::update_banner,
                    hud::update_kill_feed,
                    hud::update_scoreboard,
                    hud::update_name_tags,
                    fx::spawn_shot_effects,
                    fx::update_effects,
                    fx::draw_tracers,
                    audio::play_shot_sounds,
                    audio::play_feedback_sounds,
                    audio::play_state_sounds,
                    audio::play_remote_footsteps,
                )
                    .after(hud::receive_server_messages)
                    .run_if(in_state(AppScreen::Playing)),
            )
            .add_systems(
                FixedUpdate,
                prediction::predict_and_send.run_if(in_state(ClientState::Connected)),
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
    mut look: ResMut<LocalLook>,
    mut prediction: ResMut<Prediction>,
    mut fire: ResMut<FireQueued>,
    mut scoped: ResMut<Scoped>,
    mut sway: ResMut<ScopeSway>,
    mut health: ResMut<LocalHealth>,
    mut hit_marker: ResMut<HitMarker>,
    mut eye: ResMut<SmoothedEyeHeight>,
    camera: Single<(&mut Transform, &mut Projection), With<ViewCamera>>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
    settings: Res<Settings>,
) {
    for entity in &players {
        commands.entity(entity).despawn();
    }

    *look = LocalLook::default();
    *prediction = Prediction::default();
    *fire = FireQueued::default();
    *scoped = Scoped::default();
    *sway = ScopeSway::default();
    *health = LocalHealth::default();
    *hit_marker = HitMarker::default();
    *eye = SmoothedEyeHeight::default();

    // The menu backdrop: the static overview camera from startup.
    let (mut transform, mut projection) = camera.into_inner();
    *transform = Transform::from_xyz(0.0, 8.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y);
    if let Projection::Perspective(perspective) = &mut *projection {
        perspective.fov = settings.fov.to_radians();
    }

    cursor.grab_mode = CursorGrabMode::None;
    cursor.visible = true;
}

fn spawn_level_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // The camera (positioned each frame from our predicted player). It is also the "ears"
    // for positional audio.
    // A bright sky: the clear colour is the sky, and haze fades distant scenery into it.
    let sky = Color::srgb(0.42, 0.65, 0.92);
    commands.insert_resource(ClearColor(sky));
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.75, 0.85, 1.0),
        brightness: 350.0,
        ..default()
    });
    commands.spawn((
        ViewCamera,
        Camera3d::default(),
        DistanceFog {
            color: Color::srgb(0.7, 0.82, 0.95),
            falloff: FogFalloff::Linear {
                start: 50.0,
                end: 140.0,
            },
            ..default()
        },
        SpatialListener::new(0.4),
        Transform::from_xyz(0.0, 8.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands.spawn((
        DirectionalLight {
            color: Color::srgb(1.0, 0.96, 0.88),
            illuminance: 12_000.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(14.0, 22.0, 8.0).looking_at(Vec3::ZERO, Vec3::Y),
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
