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

/// Server -> owner only: your current health.
#[derive(Message, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct HealthUpdate {
    pub health: f32,
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
}

pub struct ProtocolPlugin;

impl Plugin for ProtocolPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Time::<Fixed>::from_hz(TICK_RATE))
            .replicate::<PlayerState>()
            .replicate::<PlayerOwner>()
            // Unordered: a newer input makes an older one irrelevant.
            .add_client_message::<PlayerInput>(Channel::Unordered)
            .add_server_message::<HealthUpdate>(Channel::Ordered)
            .add_server_message::<HitFeedback>(Channel::Ordered)
            .add_server_message::<ShotFx>(Channel::Unordered);
    }
}
