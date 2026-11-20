//! Everything the client and server must agree on: replicated components and messages.
//!
//! IMPORTANT: registration order must be identical on client and server, so both
//! apps add this one plugin.

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 5000;
/// Client and server must use the same protocol id or the handshake is rejected.
pub const PROTOCOL_ID: u64 = 0x5253_4854; // "RSHT"

/// Simulation rate. Every input frame advances a player by exactly one tick.
pub const TICK_RATE: f64 = 64.0;
pub const TICK_DT: f32 = (1.0 / TICK_RATE) as f32;

/// Authoritative state of a player, replicated server -> clients.
/// `position` is the *eye* position.
///
/// `velocity`, `grounded`, `cooldown` and `last_seq` are what a client needs to
/// re-simulate ("replay") its own unconfirmed inputs on top of the server state.
///
/// Note: health is deliberately NOT in here. Everyone receives this component, and you
/// shouldn't know how hurt other players are. Health is sent only to its owner.
#[derive(Component, Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
pub struct PlayerState {
    pub position: Vec3,
    pub velocity: Vec3,
    pub grounded: bool,
    pub yaw: f32,
    pub pitch: f32,
    /// Sequence number of the last input the server applied to this player.
    pub last_seq: u32,
    pub alive: bool,
    /// Ticks until the rifle can fire again (bolt action).
    pub cooldown: u16,
    /// Crouched or sliding: lower eye height and a smaller hitbox.
    pub crouched: bool,
    /// Ticks of slide left. While > 0 (and crouched) friction is low and steering is weak.
    pub slide_ticks: u16,
    /// Ticks until another slide may start.
    pub slide_cooldown: u16,
    /// Jump / crouch keys as of the previous tick, to detect fresh presses (no auto-bhop).
    pub jump_held: bool,
    pub crouch_held: bool,
    /// A jump press stays "remembered" for a few ticks so you can press slightly before landing.
    pub jump_buffer: u8,
    /// Rounds left in the magazine.
    pub ammo: u8,
    /// Ticks until the reload finishes (0 = not reloading).
    pub reload: u16,
}

/// Display name, replicated to everyone (shown on name tags and the scoreboard).
#[derive(Component, Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct PlayerName(pub String);

impl Default for PlayerName {
    fn default() -> Self {
        Self("Player".into())
    }
}

/// Kills and deaths in the current match, replicated to everyone for the scoreboard.
#[derive(Component, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct PlayerStats {
    pub kills: u16,
    pub deaths: u16,
}

pub const MAX_NAME_LEN: usize = 16;

/// Never trust a client-supplied name: strip control characters, trim, cap the length.
pub fn sanitize_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_LEN)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        PlayerName::default().0
    } else {
        cleaned.to_string()
    }
}

/// Which connection owns this player (the netcode client id).
/// Clients compare it to their own id to find "me".
#[derive(Component, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct PlayerOwner(pub u64);

/// One tick of player intent.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
pub struct InputFrame {
    /// Increases by 1 per tick. Lets the server ack and lets us detect duplicates.
    pub seq: u32,
    /// -1.0 (back) ..= 1.0 (forward)
    pub forward: f32,
    /// -1.0 (left) ..= 1.0 (right)
    pub right: f32,
    pub jump: bool,
    pub crouch: bool,
    pub yaw: f32,
    pub pitch: f32,
    /// The trigger was pulled this tick. Fire is part of the input so the shot happens
    /// from exactly the position the client predicted at that tick.
    pub fire: bool,
    /// Start a reload (it also starts by itself when the magazine runs empty).
    pub reload: bool,
    /// The server tick the shooter was looking at (for lag compensation).
    pub view_tick: u32,
}

impl InputFrame {
    /// Never trust the client: clamp everything and reject NaN/inf.
    pub fn sanitized(mut self) -> Self {
        let all_finite = self.forward.is_finite()
            && self.right.is_finite()
            && self.yaw.is_finite()
            && self.pitch.is_finite();
        if !all_finite {
            return Self {
                seq: self.seq,
                ..Self::default()
            };
        }
        self.forward = self.forward.clamp(-1.0, 1.0);
        self.right = self.right.clamp(-1.0, 1.0);
        self.pitch = self.pitch.clamp(-1.54, 1.54);
        self
    }
}

/// What a client sends the server every tick: its most recent few input frames.
/// The repeats make us robust to a lost packet (the server skips ones it already applied).
/// The client only reports *intent* (keys + where it is aiming), never its position.
#[derive(Message, Serialize, Deserialize, Clone, Debug, Default)]
pub struct PlayerInput {
    pub frames: Vec<InputFrame>,
}

/// Client -> server: the nickname to show.
#[derive(Message, Serialize, Deserialize, Clone, Debug)]
pub struct SetName(pub String);

/// Server -> owner only: your current health, and where the shot that hurt you came from.
#[derive(Message, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct HealthUpdate {
    pub health: f32,
    pub damage_source: Option<Vec3>,
}

/// Server -> everyone: someone was killed (for the kill feed).
#[derive(Message, Serialize, Deserialize, Clone, Debug)]
pub struct KillFeed {
    pub killer: String,
    pub victim: String,
    pub headshot: bool,
}

/// Server -> everyone: the match ended / a new one began.
#[derive(Message, Serialize, Deserialize, Clone, Debug)]
pub enum MatchEvent {
    Won { winner: String },
    Started,
}

/// Server -> shooter only: your shot hit someone.
#[derive(Message, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct HitFeedback {
    pub headshot: bool,
    pub kill: bool,
}

/// Server -> everyone: a shot was fired (so tracers can be drawn).
#[derive(Message, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct ShotFx {
    pub shooter: u64,
    pub origin: Vec3,
    pub end: Vec3,
    /// The shot struck a player (as opposed to a wall or nothing), for impact effects.
    pub hit_player: bool,
}

pub struct ProtocolPlugin;

impl Plugin for ProtocolPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Time::<Fixed>::from_hz(TICK_RATE))
            .replicate::<PlayerState>()
            .replicate::<PlayerOwner>()
            .replicate::<PlayerName>()
            .replicate::<PlayerStats>()
            // Unordered: a newer input makes an older one irrelevant.
            .add_client_message::<PlayerInput>(Channel::Unordered)
            .add_client_message::<SetName>(Channel::Ordered)
            .add_server_message::<KillFeed>(Channel::Ordered)
            .add_server_message::<MatchEvent>(Channel::Ordered)
            .add_server_message::<HealthUpdate>(Channel::Ordered)
            .add_server_message::<HitFeedback>(Channel::Ordered)
            .add_server_message::<ShotFx>(Channel::Unordered);
    }
}
