//! Mouse and keyboard handling: capturing the mouse, looking, firing, scoping and scope sway.

use bevy::input::mouse::AccumulatedMouseMotion;

use super::*;
use crate::settings::Action;

/// Left click = fire, hold right click = scope. Only counts while the mouse is captured, and
/// the click that captures the mouse is ignored (this system runs before `grab_cursor`).
pub(super) fn read_fire_and_scope(
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
pub(super) fn grab_cursor(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    pause_buttons: Query<&Interaction, With<pause::PauseButton>>,
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
) {
    // Clicking a pause-screen button must not recapture the mouse.
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

pub(super) fn look(
    motion: Res<AccumulatedMouseMotion>,
    scoped: Res<Scoped>,
    settings: Res<Settings>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut look: ResMut<LocalLook>,
) {
    if cursor.grab_mode != CursorGrabMode::Locked {
        return;
    }
    let scope_factor = if scoped.0 { SCOPED_SENSITIVITY } else { 1.0 };
    let sensitivity = MOUSE_SENSITIVITY * settings.sensitivity * scope_factor;
    look.yaw -= motion.delta.x * sensitivity;
    look.pitch = (look.pitch - motion.delta.y * sensitivity).clamp(-1.54, 1.54);
}

/// How far the crosshair wanders while scoped, in radians, at normal breathing.
const SWAY_YAW: f32 = 0.0022;
const SWAY_PITCH: f32 = 0.0026;
const BREATH_SECONDS: f32 = 4.0;
const BREATH_RECOVERY_SECONDS: f32 = 6.0;
const WINDED_SECONDS: f32 = 2.0;

/// Scoped aim drifts; holding the breath key steadies it for a few seconds, then you gasp
/// (worse sway) until you've recovered.
pub(super) fn update_sway(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<Settings>,
    scoped: Res<Scoped>,
    mut sway: ResMut<ScopeSway>,
) {
    let dt = time.delta_secs();

    if !scoped.0 {
        sway.yaw = 0.0;
        sway.pitch = 0.0;
        sway.holding = false;
        sway.winded = 0.0;
        sway.breath = (sway.breath + dt / BREATH_RECOVERY_SECONDS).min(1.0);
        return;
    }

    sway.time += dt;
    sway.winded = (sway.winded - dt).max(0.0);
    sway.holding =
        keys.pressed(settings.key(Action::HoldBreath)) && sway.winded == 0.0 && sway.breath > 0.0;

    if sway.holding {
        sway.breath = (sway.breath - dt / BREATH_SECONDS).max(0.0);
        if sway.breath == 0.0 {
            sway.winded = WINDED_SECONDS;
            sway.holding = false;
        }
    } else {
        sway.breath = (sway.breath + dt / BREATH_RECOVERY_SECONDS).min(1.0);
    }

    let strength = if sway.holding {
        0.1
    } else if sway.winded > 0.0 {
        1.6
    } else {
        1.0
    };
    let t = sway.time;
    sway.yaw = SWAY_YAW * strength * ((t * 1.1).sin() + 0.5 * (t * 2.7 + 1.0).sin());
    sway.pitch = SWAY_PITCH * strength * ((t * 1.5 + 0.5).sin() + 0.5 * (t * 3.1).sin());
}
