//! Headless authoritative server: owns all player state, simulates movement,
//! resolves shots (with lag compensation), applies damage and respawns.
//!
//! Movement is *input-driven*: every input frame a client sends advances that player
//! by exactly one tick, in order. The state carries `last_seq` so the client knows
//! which of its inputs the server has already applied.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::SystemTime;

use bevy::prelude::*;
use bevy_replicon::{
    prelude::*,
    server::server_tick::ServerTick,
    shared::backend::connected_client::{NetworkId, NetworkIdMap},
};
use bevy_replicon_renet::{
    RenetChannelsExt, RenetServer,
    netcode::{NetcodeServerTransport, ServerAuthentication, ServerConfig},
    renet::ConnectionConfig,
};

use crate::combat::{
    BODY_DAMAGE, HEAD_DAMAGE, MAX_HEALTH, MAX_REWIND_TICKS, RESPAWN_TICKS, Target, aim_direction,
    trace,
};
use crate::player::{eye_height, step_player};
use crate::protocol::{
    HealthUpdate, HitFeedback, PROTOCOL_ID, PlayerInput, PlayerOwner, PlayerState, ShotFx,
};
use crate::world::{LevelGeometry, spawn_points};

const MAX_CLIENTS: usize = 16;
/// Most frames we read from a single message.
const MAX_FRAMES_PER_MESSAGE: usize = 8;
/// Anti speed-hack: a player earns one input frame of "time" per server tick and can
/// bank a few for network jitter. Frames sent faster than real time are dropped.
const MAX_INPUT_BUDGET: f32 = 12.0;
/// Ticks of position history kept for lag compensation.
const HISTORY_LEN: usize = 64;

/// How many input frames this player may still have processed right now.
#[derive(Component)]
struct InputBudget(f32);

/// Server-only health (never replicated; the owner gets it via `HealthUpdate`).
#[derive(Component)]
struct Health(f32);

/// Counts down to respawn while dead.
#[derive(Component)]
struct RespawnTimer(u32);

/// Where this player's feet were (and whether they were crouched) at the end of each recent
/// tick, newest last. Used to rewind players to what a lagging shooter was seeing.
#[derive(Component, Default)]
struct PositionHistory(VecDeque<(u32, Vec3, bool)>);

#[derive(Resource)]
struct ServerPort(u16);

#[derive(Resource, Default)]
struct NextSpawn(usize);

pub struct GameServerPlugin {
    pub port: u16,
}

impl Plugin for GameServerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ServerPort(self.port))
            .init_resource::<NextSpawn>()
            .add_systems(Startup, start_server)
            .add_observer(spawn_player)
            .add_observer(despawn_player)
            .add_systems(
                PreUpdate,
                receive_inputs
                    .after(ServerSystems::Receive)
                    .run_if(in_state(ServerState::Running)),
            )
            .add_systems(
                FixedUpdate,
                (refill_budgets, record_history, tick_respawns)
                    .chain()
                    .run_if(in_state(ServerState::Running)),
            );
    }
}

fn start_server(
    mut commands: Commands,
    channels: Res<RepliconChannels>,
    port: Res<ServerPort>,
) -> Result<()> {
    let port = port.0;
    info!("starting server on UDP port {port}");

    let server = RenetServer::new(ConnectionConfig {
        server_channels_config: channels.server_configs(),
        client_channels_config: channels.client_configs(),
        ..Default::default()
    });

    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
    let server_config = ServerConfig {
        current_time,
        max_clients: MAX_CLIENTS,
        protocol_id: PROTOCOL_ID,
        // Dev only! Public servers need `ServerAuthentication::Secure` with connect tokens.
        authentication: ServerAuthentication::Unsecure,
        public_addresses: Default::default(),
    };
    let transport = NetcodeServerTransport::new(server_config, socket)?;

    commands.insert_resource(server);
    commands.insert_resource(transport);
    Ok(())
}

/// Picks spawn points in rotation so players don't start on top of each other.
fn next_spawn(counter: &mut NextSpawn) -> Vec3 {
    let points = spawn_points();
    let point = points[counter.0 % points.len()];
    counter.0 += 1;
    point
}

/// A client finished connecting: give it a player.
fn spawn_player(
    add: On<Add, AuthorizedClient>,
    mut commands: Commands,
    ids: Query<&NetworkId>,
    mut spawns: ResMut<NextSpawn>,
    mut health_out: MessageWriter<ToClients<HealthUpdate>>,
) {
    let Ok(id) = ids.get(add.entity) else {
        return;
    };
    info!("player joined (client id {})", id.get());

    commands.spawn((
        Replicated,
        PlayerOwner(id.get()),
        PlayerState {
            position: next_spawn(&mut spawns),
            alive: true,
            ..default()
        },
        Health(MAX_HEALTH),
        InputBudget(MAX_INPUT_BUDGET),
        PositionHistory::default(),
    ));

    health_out.write(ToClients {
        targets: SendTargets::Single(ClientId::Client(add.entity)),
        message: HealthUpdate { health: MAX_HEALTH },
    });
}

/// A client disconnected: remove its player (this also despawns it on every client).
fn despawn_player(
    remove: On<Remove, ConnectedClient>,
    mut commands: Commands,
    ids: Query<&NetworkId>,
    players: Query<(Entity, &PlayerOwner)>,
) {
    let Ok(id) = ids.get(remove.entity) else {
        return;
    };
    info!("player left (client id {})", id.get());

    for (entity, owner) in &players {
        if owner.0 == id.get() {
            commands.entity(entity).despawn();
        }
    }
}

fn refill_budgets(mut budgets: Query<&mut InputBudget>) {
    for mut budget in &mut budgets {
        budget.0 = (budget.0 + 1.0).min(MAX_INPUT_BUDGET);
    }
}

/// Remember where everyone was this tick, for lag compensation.
fn record_history(tick: Res<ServerTick>, mut players: Query<(&PlayerState, &mut PositionHistory)>) {
    let now = tick.get();
    for (state, mut history) in &mut players {
        let feet = state.position - Vec3::Y * eye_height(state.crouched);
        history.0.push_back((now, feet, state.crouched));
        while history.0.len() > HISTORY_LEN {
            history.0.pop_front();
        }
    }
}

/// Dead players respawn after a delay: full health, new spawn point.
fn tick_respawns(
    mut commands: Commands,
    mut spawns: ResMut<NextSpawn>,
    mut dead: Query<(Entity, &PlayerOwner, &mut RespawnTimer, &mut PlayerState, &mut Health)>,
    network_ids: Res<NetworkIdMap>,
    mut health_out: MessageWriter<ToClients<HealthUpdate>>,
) {
    for (entity, owner, mut timer, mut state, mut health) in &mut dead {
        timer.0 = timer.0.saturating_sub(1);
        if timer.0 > 0 {
            continue;
        }

        health.0 = MAX_HEALTH;
        state.position = next_spawn(&mut spawns);
        state.velocity = Vec3::ZERO;
        state.alive = true;
        state.cooldown = 0;
        state.crouched = false;
        state.slide_ticks = 0;
        state.slide_cooldown = 0;
        state.jump_buffer = 0;
        commands.entity(entity).remove::<RespawnTimer>();

        if let Some(&client) = network_ids.get(&NetworkId::new(owner.0)) {
            health_out.write(ToClients {
                targets: SendTargets::Single(ClientId::Client(client)),
                message: HealthUpdate { health: MAX_HEALTH },
            });
        }
    }
}

/// Where `history` says a player's feet were at `tick` (nearest recorded tick), and whether
/// they were crouched. Falls back to the current state if there's no history yet.
fn feet_at(history: &PositionHistory, tick: u32, current: &PlayerState) -> (Vec3, bool) {
    history
        .0
        .iter()
        .min_by_key(|(t, _, _)| t.abs_diff(tick))
        .map(|(_, feet, crouched)| (*feet, *crouched))
        .unwrap_or((
            current.position - Vec3::Y * eye_height(current.crouched),
            current.crouched,
        ))
}

/// Applies each *new* input frame, in order, exactly once, and resolves any shots.
fn receive_inputs(
    mut commands: Commands,
    mut inputs: MessageReader<FromClient<PlayerInput>>,
    level: Res<LevelGeometry>,
    tick: Res<ServerTick>,
    network_ids: Res<NetworkIdMap>,
    ids: Query<&NetworkId>,
    mut players: Query<(
        Entity,
        &PlayerOwner,
        &mut PlayerState,
        &mut InputBudget,
        &mut Health,
        &PositionHistory,
    )>,
    mut health_out: MessageWriter<ToClients<HealthUpdate>>,
    mut hit_out: MessageWriter<ToClients<HitFeedback>>,
    mut fx_out: MessageWriter<ToClients<ShotFx>>,
) {
    let now = tick.get();

    for input in inputs.read() {
        let ClientId::Client(client) = input.client_id else {
            continue;
        };
        let Ok(id) = ids.get(client) else {
            continue;
        };

        // Find the shooter's entity first; shots need to look at *other* players too,
        // so we collect what we need and avoid holding two mutable borrows.
        let Some(shooter) = players
            .iter()
            .find(|(_, owner, ..)| owner.0 == id.get())
            .map(|(entity, ..)| entity)
        else {
            continue;
        };

        for frame in input.message.frames.iter().take(MAX_FRAMES_PER_MESSAGE) {
            let frame = frame.sanitized();

            let (fired, origin, yaw, pitch) = {
                let Ok((_, _, mut state, mut budget, ..)) = players.get_mut(shooter) else {
                    break;
                };
                // Already applied (we got it redundantly) or out of order.
                if frame.seq <= state.last_seq {
                    continue;
                }
                if budget.0 < 1.0 {
                    // Sending faster than real time: drop it.
                    break;
                }
                budget.0 -= 1.0;

                let fired = step_player(&mut state, &frame, &level.0);
                (fired, state.position, state.yaw, state.pitch)
            };

            if !fired {
                continue;
            }

            // Lag compensation: the shooter saw other players as they were `view_tick`.
            // Clamp so a cheater can't claim to be seeing the distant past (or future).
            let view_tick = frame
                .view_tick
                .clamp(now.saturating_sub(MAX_REWIND_TICKS), now);

            let targets: Vec<Target> = players
                .iter()
                .filter(|(entity, _, state, ..)| *entity != shooter && state.alive)
                .map(|(entity, _, state, _, _, history)| {
                    let (feet, crouched) = feet_at(history, view_tick, state);
                    Target {
                        id: entity,
                        feet,
                        crouched,
                    }
                })
                .collect();

            let dir = aim_direction(yaw, pitch);
            let result = trace(origin, dir, &level.0, &targets);

            // Tracer for everyone *except the shooter*, who already drew their own instantly.
            fx_out.write(ToClients {
                targets: SendTargets::AllExcept(ClientId::Client(client)),
                message: ShotFx {
                    shooter: id.get(),
                    origin,
                    end: origin + dir * result.end_distance,
                },
            });

            let Some(hit) = result.hit else {
                continue;
            };

            let damage = if hit.headshot { HEAD_DAMAGE } else { BODY_DAMAGE };
            let mut killed = false;
            if let Ok((victim, owner, mut state, _, mut health, _)) = players.get_mut(hit.target) {
                health.0 = (health.0 - damage).max(0.0);
                killed = health.0 <= 0.0;

                if killed {
                    state.alive = false;
                    commands.entity(victim).insert(RespawnTimer(RESPAWN_TICKS));
                    info!("player {} was killed by {}", owner.0, id.get());
                }

                if let Some(&victim_client) = network_ids.get(&NetworkId::new(owner.0)) {
                    health_out.write(ToClients {
                        targets: SendTargets::Single(ClientId::Client(victim_client)),
                        message: HealthUpdate { health: health.0 },
                    });
                }
            }

            hit_out.write(ToClients {
                targets: SendTargets::Single(ClientId::Client(client)),
                message: HitFeedback {
                    headshot: hit.headshot,
                    kill: killed,
                },
            });
        }
    }
}
