//! The pause screen: shown while the mouse is released. Resume, change settings, rebind keys
//! or leave the game.

use super::*;
use crate::settings::{Action, CrosshairStyle, FOV_RANGE, SENSITIVITY_RANGE, key_label, pressed_bindable_key};

use super::hud::HudEntity;

const BUTTON: Color = Color::srgb(0.2, 0.22, 0.28);
const BUTTON_HOVER: Color = Color::srgb(0.3, 0.33, 0.42);
const BUTTON_ACTIVE: Color = Color::srgb(0.55, 0.45, 0.15);
const DISCONNECT: Color = Color::srgb(0.55, 0.18, 0.18);
const DISCONNECT_HOVER: Color = Color::srgb(0.7, 0.25, 0.25);

#[derive(Component)]
pub(super) struct PauseOverlay;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Setting {
    Sensitivity,
    Fov,
    Volume,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PauseAction {
    Disconnect,
    Less(Setting),
    More(Setting),
    Crosshair,
    Rebind(Action),
}

/// Marks every clickable element of the pause screen (also keeps clicks from recapturing the mouse).
#[derive(Component)]
pub(super) struct PauseButton(PauseAction);

/// The colour a button has when not hovered.
#[derive(Component)]
pub(super) struct BaseColor(Color);

/// A text that shows the current value of a setting or binding.
#[derive(Component)]
pub(super) struct ValueText(PauseAction);

/// The number between a stepper's - and + buttons.
#[derive(Component)]
pub(super) struct StepValue(Setting);

/// The action waiting for its new key, if any.
#[derive(Resource, Default)]
pub(super) struct Rebinding(Option<Action>);

fn text(label: &str, size: f32) -> impl Bundle {
    (
        Text::new(label),
        TextFont {
            font_size: size,
            ..default()
        },
        TextColor(Color::WHITE),
    )
}

fn button(action: PauseAction, width: f32, color: Color, label: &str) -> impl Bundle {
    (
        PauseButton(action),
        BaseColor(color),
        Button,
        Node {
            width: Val::Px(width),
            height: Val::Px(30.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(color),
        children![(ValueText(action), text(label, 18.0))],
    )
}

fn row(label: &str, content: impl Bundle) -> impl Bundle {
    (
        Node {
            width: Val::Percent(100.0),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            column_gap: Val::Px(8.0),
            ..default()
        },
        children![text(label, 18.0), content],
    )
}

fn stepper(setting: Setting) -> impl Bundle {
    (
        Node {
            align_items: AlignItems::Center,
            column_gap: Val::Px(6.0),
            ..default()
        },
        children![
            button(PauseAction::Less(setting), 30.0, BUTTON, "-"),
            (
                Node {
                    width: Val::Px(70.0),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                children![(StepValue(setting), text("", 18.0))],
            ),
            button(PauseAction::More(setting), 30.0, BUTTON, "+"),
        ],
    )
}

pub(super) fn spawn_pause_overlay(mut commands: Commands) {
    let mut panel = commands.spawn((
        PauseOverlay,
        HudEntity,
        Visibility::Visible,
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(340.0),
            margin: UiRect {
                left: Val::Px(-170.0),
                top: Val::Px(-250.0),
                ..default()
            },
            padding: UiRect::all(Val::Px(16.0)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(8.0),
            ..default()
        },
        children![
            text("PAUSED", 34.0),
            text("Click anywhere else to resume", 16.0),
            row("Sensitivity", stepper(Setting::Sensitivity)),
            row("Field of view", stepper(Setting::Fov)),
            row("Volume", stepper(Setting::Volume)),
            row(
                "Crosshair",
                button(PauseAction::Crosshair, 110.0, BUTTON, "")
            ),
        ],
    ));

    panel.with_children(|parent| {
        parent.spawn((
            text("Key bindings", 20.0),
            Node {
                margin: UiRect::top(Val::Px(6.0)),
                ..default()
            },
        ));
        for action in Action::ALL {
            parent.spawn(row(
                action.label(),
                button(PauseAction::Rebind(action), 110.0, BUTTON, ""),
            ));
        }
        parent.spawn(button(PauseAction::Disconnect, 180.0, DISCONNECT, "Disconnect"));
    });
}

pub(super) fn pause_overlay(
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut overlay: Single<&mut Visibility, With<PauseOverlay>>,
) {
    **overlay = if cursor.grab_mode == CursorGrabMode::Locked {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
}

fn step(value: f32, range: (f32, f32), increment: f32, direction: f32) -> f32 {
    (value + increment * direction).clamp(range.0, range.1)
}

fn adjust(settings: &mut Settings, setting: Setting, direction: f32) {
    match setting {
        Setting::Sensitivity => {
            settings.sensitivity = step(settings.sensitivity, SENSITIVITY_RANGE, 0.1, direction)
        }
        Setting::Fov => settings.fov = step(settings.fov, FOV_RANGE, 5.0, direction),
        Setting::Volume => settings.volume = step(settings.volume, (0.0, 1.0), 0.1, direction),
    }
}

pub(super) fn handle_buttons(
    mut commands: Commands,
    buttons: Query<(&Interaction, &PauseButton), Changed<Interaction>>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut settings: ResMut<Settings>,
    mut rebinding: ResMut<Rebinding>,
    mut link: ResMut<Link>,
    mut transport: Option<ResMut<NetcodeClientTransport>>,
) {
    if cursor.grab_mode == CursorGrabMode::Locked {
        return;
    }
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match button.0 {
            PauseAction::Disconnect => {
                // Tell the server we're leaving, then drop the connection; `track_connection`
                // notices and returns us to the menu.
                link.user_requested = true;
                if let Some(transport) = transport.as_mut() {
                    transport.disconnect();
                }
                commands.remove_resource::<RenetClient>();
                commands.remove_resource::<NetcodeClientTransport>();
            }
            PauseAction::Less(setting) => {
                adjust(&mut settings, setting, -1.0);
                settings.save();
            }
            PauseAction::More(setting) => {
                adjust(&mut settings, setting, 1.0);
                settings.save();
            }
            PauseAction::Crosshair => {
                settings.crosshair = settings.crosshair.next();
                settings.save();
            }
            PauseAction::Rebind(action) => rebinding.0 = Some(action),
        }
    }
}

/// While a binding is being changed: the next key pressed becomes the binding, Escape cancels.
pub(super) fn capture_rebind(
    keys: Res<ButtonInput<KeyCode>>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut rebinding: ResMut<Rebinding>,
    mut settings: ResMut<Settings>,
) {
    let Some(action) = rebinding.0 else {
        return;
    };
    if cursor.grab_mode == CursorGrabMode::Locked {
        rebinding.0 = None;
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        rebinding.0 = None;
    } else if let Some(key) = pressed_bindable_key(&keys) {
        settings.bind(action, key);
        settings.save();
        rebinding.0 = None;
    }
}

fn crosshair_label(style: CrosshairStyle) -> &'static str {
    style.label()
}

pub(super) fn refresh_texts(
    settings: Res<Settings>,
    rebinding: Res<Rebinding>,
    mut texts: Query<(&ValueText, &mut Text), Without<StepValue>>,
    mut steps: Query<(&StepValue, &mut Text), Without<ValueText>>,
) {
    if !settings.is_changed() && !rebinding.is_changed() {
        return;
    }
    for (value, mut text) in &mut steps {
        let label = match value.0 {
            Setting::Sensitivity => format!("{:.1}x", settings.sensitivity),
            Setting::Fov => format!("{:.0}", settings.fov),
            Setting::Volume => format!("{:.0}%", settings.volume * 100.0),
        };
        if text.0 != label {
            text.0 = label;
        }
    }
    for (value, mut text) in &mut texts {
        let label = match value.0 {
            PauseAction::Crosshair => crosshair_label(settings.crosshair).to_string(),
            PauseAction::Rebind(action) => {
                if rebinding.0 == Some(action) {
                    "press a key".to_string()
                } else {
                    key_label(settings.key(action))
                }
            }
            // Static labels ("-", "+", "Disconnect") keep their text.
            _ => continue,
        };
        if text.0 != label {
            text.0 = label;
        }
    }
}

pub(super) fn button_colors(
    rebinding: Res<Rebinding>,
    mut buttons: Query<(&Interaction, &PauseButton, &BaseColor, &mut BackgroundColor)>,
) {
    for (interaction, button, base, mut background) in &mut buttons {
        let waiting = matches!(button.0, PauseAction::Rebind(a) if rebinding.0 == Some(a));
        let color = match (*interaction, button.0) {
            _ if waiting => BUTTON_ACTIVE,
            (Interaction::None, _) => base.0,
            (_, PauseAction::Disconnect) => DISCONNECT_HOVER,
            _ => BUTTON_HOVER,
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}
