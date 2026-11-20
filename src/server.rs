//! Headless authoritative server: owns all player state, simulates movement,
//! resolves shots (with lag compensation), applies damage, tracks the score and runs the
//! match (respawns, winner, next round).
//!
//! Movement is *input-driven*: every input frame a client sends advances that player
//! by exactly one tick, in order. The state carries `last_seq` so the client knows
//! which of its inputs the server has already applied.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::SystemTime;

use bevy::ecs::system::SystemParam;
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
    BODY_DAMAGE, HEAD_DAMAGE, INTERMISSION_TICKS, MAGAZINE_SIZE, MAX_HEALTH, MAX_REWIND_TICKS,
    RESPAWN_TICKS, SCORE_LIMIT, SPAWN_PROTECTION_TICKS, Target, aim_direction, trace,
};
use crate::player::{eye_height, step_player};
use crate::protocol::{
    HealthUpdate, HitFeedback, KillFeed, MatchEvent, PROTOCOL_ID, PlayerInput, PlayerName,
    PlayerOwner, PlayerState, PlayerStats, SetName, ShotFx, sanitize_name,
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

/// Ticks of damage immunity left after (re)spawning.
#[derive(Component)]
struct Protection(u32);

/// Where this player's feet were (and whether they were crouched) at the end of each recent
/// tick, newest last. Used to rewind players to what a lagging shooter was seeing.
#[derive(Component, Default)]
struct PositionHistory(VecDeque<(u32, Vec3, bool)>);

#[derive(Resource)]
struct ServerPort(u16);

/// Rotates which spawn point wins a tie, so spawns vary when nobody is around.
#[derive(Resource, Default)]
struct NextSpawn(usize);

/// Whether a match is running, or the winner screen is up (shots are ignored then).
#[derive(Resource, Default, PartialEq, Eq, Debug)]
enum MatchPhase {
    #[default]
    Playing,
    Intermission(u32),
}

/// Everything the server sends out, bundled so systems stay readable.
#[derive(SystemParam)]
struct Outbox<'w> {
    health: MessageWriter<'w, ToClients<HealthUpdate>>,
    hits: MessageWriter<'w, ToClients<HitFeedback>>,
    shots: MessageWriter<'w, ToClients<ShotFx>>,
    kills: MessageWriter<'w, ToClients<KillFeed>>,
    matches: MessageWriter<'w, ToClients<MatchEvent>>,
}

impl Outbox<'_> {
    fn send_health(
        &mut self,
        network_ids: &NetworkIdMap,
        owner: u64,
        health: f32,
        damage_source: Option<Vec3>,
    ) {
        if let Some(&client) = network_ids.get(&NetworkId::new(owner)) {
            self.health.write(ToClients {
                targets: SendTargets::Single(ClientId::Client(client)),
                message: HealthUpdate {
                    health,
                    damage_source,
                },
            });
        }
    }
}

pub struct GameServerPlugin {
    pub port: u16,
}

impl Plugin for GameServerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ServerPort(self.port))
            .init_resource::<NextSpawn>()
            .init_resource::<MatchPhase>()
            .add_systems(Startup, start_server)
            .add_observer(spawn_player)
            .add_observer(despawn_player)
            .add_systems(
                PreUpdate,
                (receive_names, receive_inputs)
                    .chain()
                    .after(ServerSystems::Receive)
                    .run_if(in_state(ServerState::Running)),
            )
            .add_systems(
                FixedUpdate,
                (
                    refill_budgets,
                    record_history,
                    tick_protection,
                    tick_respawns,
                    tick_match,
                )
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

/// Picks the spawn point farthest from every enemy (so you don't spawn in someone's sights).
/// With nobody around every point ties, and `rotation` decides, so spawns still vary.
fn pick_spawn(candidates: &[Vec3], enemies: &[Vec3], rotation: usize) -> Vec3 {
    let mut best = candidates[rotation % candidates.len()];
    let mut best_distance = f32::NEG_INFINITY;
    for offset in 0..candidates.len() {
        let point = candidates[(rotation + offset) % candidates.len()];
        let nearest = enemies
            .iter()
            .map(|enemy| (*enemy - point).with_y(0.0).length())
            .fold(f32::INFINITY, f32::min);
        if nearest > best_distance {
            best_distance = nearest;
            best = point;
        }
    }
    best
}

fn choose_spawn<'a>(
    counter: &mut NextSpawn,
    enemies: impl IntoIterator<Item = &'a PlayerState>,
) -> Vec3 {
    let positions: Vec<Vec3> = enemies
        .into_iter()
        .filter(|state| state.alive)
        .map(|state| state.position)
        .collect();
    counter.0 += 1;
    pick_spawn(&spawn_points(), &positions, counter.0)
}

/// Resets a player for a new life, keeping the bookkeeping the client's prediction relies on.
fn respawn(state: &mut PlayerState, health: &mut Health, protection: &mut Protection, at: Vec3) {
    health.0 = MAX_HEALTH;
    protection.0 = SPAWN_PROTECTION_TICKS;
    *state = PlayerState {
        position: at,
        alive: true,
        ammo: MAGAZINE_SIZE,
        last_seq: state.last_seq,
        yaw: state.yaw,
        pitch: state.pitch,
        jump_held: state.jump_held,
        crouch_held: state.crouch_held,
        ..default()
    };
}

/// A client finished connecting: give it a player.
fn spawn_player(
    add: On<Add, AuthorizedClient>,
    mut commands: Commands,
    ids: Query<&NetworkId>,
    network_ids: Res<NetworkIdMap>,
    mut spawns: ResMut<NextSpawn>,
    others: Query<&PlayerState>,
    mut out: Outbox,
) {
    let Ok(id) = ids.get(add.entity) else {
        return;
    };
    info!("player joined (client id {})", id.get());

    commands.spawn((
        Replicated,
        PlayerOwner(id.get()),
        PlayerName::default(),
        PlayerStats::default(),
        PlayerState {
            position: choose_spawn(&mut spawns, &others),
            alive: true,
            ammo: MAGAZINE_SIZE,
            ..default()
        },
        Health(MAX_HEALTH),
        Protection(SPAWN_PROTECTION_TICKS),
        InputBudget(MAX_INPUT_BUDGET),
        PositionHistory::default(),
    ));

    out.send_health(&network_ids, id.get(), MAX_HEALTH, None);
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

fn receive_names(
    mut commands: Commands,
    mut names: MessageReader<FromClient<SetName>>,
    ids: Query<&NetworkId>,
    players: Query<(Entity, &PlayerOwner, &PlayerName)>,
) {
    for message in names.read() {
        let ClientId::Client(client) = message.client_id else {
            continue;
        };
        let Ok(id) = ids.get(client) else {
            continue;
        };
        let name = sanitize_name(&message.message.0);
        for (entity, owner, current) in &players {
            if owner.0 == id.get() && current.0 != name {
                commands.entity(entity).insert(PlayerName(name.clone()));
            }
        }
    }
}

fn refill_budgets(mut budgets: Query<&mut InputBudget>) {
    for mut budget in &mut budgets {
        budget.0 = (budget.0 + 1.0).min(MAX_INPUT_BUDGET);
    }
}

fn tick_protection(mut players: Query<&mut Protection>) {
    for mut protection in &mut players {
        protection.0 = protection.0.saturating_sub(1);
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

/// Dead players respawn after a delay: full health, a spawn point away from the others.
fn tick_respawns(
    mut commands: Commands,
    mut spawns: ResMut<NextSpawn>,
    mut dead: Query<(
        Entity,
        &PlayerOwner,
        &mut RespawnTimer,
        &mut PlayerState,
        &mut Health,
        &mut Protection,
    )>,
    living: Query<&PlayerState, Without<RespawnTimer>>,
    network_ids: Res<NetworkIdMap>,
    mut out: Outbox,
) {
    for (entity, owner, mut timer, mut state, mut health, mut protection) in &mut dead {
        timer.0 = timer.0.saturating_sub(1);
        if timer.0 > 0 {
            continue;
        }

        let at = choose_spawn(&mut spawns, &living);
        respawn(&mut state, &mut health, &mut protection, at);
        commands.entity(entity).remove::<RespawnTimer>();
        out.send_health(&network_ids, owner.0, MAX_HEALTH, None);
    }
}

/// Counts down the winner screen, then starts a fresh match: scores reset, everyone respawns.
fn tick_match(
    mut commands: Commands,
    mut phase: ResMut<MatchPhase>,
    mut spawns: ResMut<NextSpawn>,
    mut players: Query<(
        Entity,
        &PlayerOwner,
        &mut PlayerState,
        &mut Health,
        &mut Protection,
        &mut PlayerStats,
    )>,
    network_ids: Res<NetworkIdMap>,
    mut out: Outbox,
) {
    let MatchPhase::Intermission(ticks) = *phase else {
        return;
    };
    if ticks > 0 {
        *phase = MatchPhase::Intermission(ticks - 1);
        return;
    }

    let mut taken: Vec<PlayerState> = Vec::new();
    for (entity, owner, mut state, mut health, mut protection, mut stats) in &mut players {
        let at = choose_spawn(&mut spawns, &taken);
        respawn(&mut state, &mut health, &mut protection, at);
        *stats = PlayerStats::default();
        taken.push(*state);
        commands.entity(entity).remove::<RespawnTimer>();
        out.send_health(&network_ids, owner.0, MAX_HEALTH, None);
    }
    out.matches.write(ToClients {
        targets: SendTargets::All,
        message: MatchEvent::Started,
    });
    *phase = MatchPhase::Playing;
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
    mut phase: ResMut<MatchPhase>,
    mut players: Query<(
        Entity,
        &PlayerOwner,
        &mut PlayerState,
        &mut InputBudget,
        &mut Health,
        &PositionHistory,
    )>,
    mut scores: Query<(&PlayerName, &mut PlayerStats, &Protection)>,
    mut out: Outbox,
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

            if !fired || *phase != MatchPhase::Playing {
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
            out.shots.write(ToClients {
                targets: SendTargets::AllExcept(ClientId::Client(client)),
                message: ShotFx {
                    shooter: id.get(),
                    origin,
                    end: origin + dir * result.end_distance,
                    hit_player: result.hit.is_some(),
                },
            });

            let Some(hit) = result.hit else {
                continue;
            };

            // Spawn protection: the shot is wasted and the shooter gets no hit feedback.
            if scores.get(hit.target).is_ok_and(|(_, _, protection)| protection.0 > 0) {
                continue;
            }

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

                out.send_health(&network_ids, owner.0, health.0, Some(origin));
            }

            if killed {
                record_kill(&mut scores, shooter, hit.target, hit.headshot, &mut phase, &mut out);
            }

            out.hits.write(ToClients {
                targets: SendTargets::Single(ClientId::Client(client)),
                message: HitFeedback {
                    headshot: hit.headshot,
                    kill: killed,
                },
            });
        }
    }
}

/// Updates the score for a kill, announces it, and ends the match at the score limit.
fn record_kill(
    scores: &mut Query<(&PlayerName, &mut PlayerStats, &Protection)>,
    killer: Entity,
    victim: Entity,
    headshot: bool,
    phase: &mut MatchPhase,
    out: &mut Outbox,
) {
    let mut victim_name = String::new();
    if let Ok((name, mut stats, _)) = scores.get_mut(victim) {
        stats.deaths += 1;
        victim_name = name.0.clone();
    }
    let Ok((name, mut stats, _)) = scores.get_mut(killer) else {
        return;
    };
    stats.kills += 1;
    let killer_name = name.0.clone();

    out.kills.write(ToClients {
        targets: SendTargets::All,
        message: KillFeed {
            killer: killer_name.clone(),
            victim: victim_name,
            headshot,
        },
    });

    if stats.kills >= SCORE_LIMIT && *phase == MatchPhase::Playing {
        *phase = MatchPhase::Intermission(INTERMISSION_TICKS);
        out.matches.write(ToClients {
            targets: SendTargets::All,
            message: MatchEvent::Won {
                winner: killer_name,
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points() -> Vec<Vec3> {
        vec![
            Vec3::new(-20.0, 1.7, 0.0),
            Vec3::new(0.0, 1.7, 0.0),
            Vec3::new(20.0, 1.7, 0.0),
        ]
    }

    #[test]
    fn spawn_is_as_far_from_enemies_as_possible() {
        let enemies = [Vec3::new(18.0, 1.7, 0.0)];
        assert_eq!(pick_spawn(&points(), &enemies, 0), Vec3::new(-20.0, 1.7, 0.0));
        let enemies = [Vec3::new(-18.0, 1.7, 0.0), Vec3::new(18.0, 1.7, 0.0)];
        assert_eq!(pick_spawn(&points(), &enemies, 0), Vec3::new(0.0, 1.7, 0.0));
    }

    #[test]
    fn spawn_rotates_when_nobody_is_around() {
        let a = pick_spawn(&points(), &[], 0);
        let b = pick_spawn(&points(), &[], 1);
        assert_ne!(a, b);
    }
}
