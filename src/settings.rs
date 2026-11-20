//! Player settings: mouse sensitivity, field of view, volume, crosshair, nickname and keybinds.
//!
//! Saved as `key=value` lines in `settings.cfg` next to where you run the game, so you can
//! also edit it by hand. Unknown or invalid lines are ignored and fall back to the defaults.

use std::fs;

use bevy::prelude::*;

const SETTINGS_FILE: &str = "settings.cfg";

pub const SENSITIVITY_RANGE: (f32, f32) = (0.2, 4.0);
pub const FOV_RANGE: (f32, f32) = (60.0, 110.0);

/// Things a key can be bound to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Crouch,
    Reload,
    HoldBreath,
    Scoreboard,
}

impl Action {
    pub const ALL: [Action; 9] = [
        Action::Forward,
        Action::Back,
        Action::Left,
        Action::Right,
        Action::Jump,
        Action::Crouch,
        Action::Reload,
        Action::HoldBreath,
        Action::Scoreboard,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::Forward => "Forward",
            Action::Back => "Back",
            Action::Left => "Left",
            Action::Right => "Right",
            Action::Jump => "Jump",
            Action::Crouch => "Crouch / slide",
            Action::Reload => "Reload",
            Action::HoldBreath => "Hold breath",
            Action::Scoreboard => "Scoreboard",
        }
    }

    fn default_key(self) -> KeyCode {
        match self {
            Action::Forward => KeyCode::KeyW,
            Action::Back => KeyCode::KeyS,
            Action::Left => KeyCode::KeyA,
            Action::Right => KeyCode::KeyD,
            Action::Jump => KeyCode::Space,
            Action::Crouch => KeyCode::ControlLeft,
            Action::Reload => KeyCode::KeyR,
            Action::HoldBreath => KeyCode::ShiftLeft,
            Action::Scoreboard => KeyCode::Tab,
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|a| *a == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CrosshairStyle {
    Dot,
    Cross,
    Off,
}

impl CrosshairStyle {
    pub fn label(self) -> &'static str {
        match self {
            CrosshairStyle::Dot => "Dot",
            CrosshairStyle::Cross => "Cross",
            CrosshairStyle::Off => "Off",
        }
    }

    pub fn next(self) -> Self {
        match self {
            CrosshairStyle::Dot => CrosshairStyle::Cross,
            CrosshairStyle::Cross => CrosshairStyle::Off,
            CrosshairStyle::Off => CrosshairStyle::Dot,
        }
    }
}

/// Keys that may be bound (mouse buttons are fixed: left fires, right scopes).
const BINDABLE: &[KeyCode] = &[
    KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE, KeyCode::KeyF,
    KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI, KeyCode::KeyJ, KeyCode::KeyK, KeyCode::KeyL,
    KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO, KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR,
    KeyCode::KeyS, KeyCode::KeyT, KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX,
    KeyCode::KeyY, KeyCode::KeyZ, KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2,
    KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7,
    KeyCode::Digit8, KeyCode::Digit9, KeyCode::Space, KeyCode::Tab, KeyCode::CapsLock,
    KeyCode::ShiftLeft, KeyCode::ShiftRight, KeyCode::ControlLeft, KeyCode::ControlRight,
    KeyCode::AltLeft, KeyCode::ArrowUp, KeyCode::ArrowDown, KeyCode::ArrowLeft,
    KeyCode::ArrowRight,
];

/// The bindable key that was pressed this frame, if any.
pub fn pressed_bindable_key(keys: &ButtonInput<KeyCode>) -> Option<KeyCode> {
    BINDABLE.iter().copied().find(|key| keys.just_pressed(*key))
}

pub fn key_label(key: KeyCode) -> String {
    let name = format!("{key:?}");
    name.strip_prefix("Key")
        .or_else(|| name.strip_prefix("Digit"))
        .unwrap_or(&name)
        .to_string()
}

#[derive(Resource, Clone, PartialEq, Debug)]
pub struct Settings {
    /// Multiplier on the base mouse sensitivity.
    pub sensitivity: f32,
    /// Vertical field of view in degrees (unscoped).
    pub fov: f32,
    /// Master volume, 0.0..=1.0.
    pub volume: f32,
    pub crosshair: CrosshairStyle,
    pub name: String,
    keys: [KeyCode; Action::ALL.len()],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sensitivity: 1.0,
            fov: 70.0,
            volume: 0.7,
            crosshair: CrosshairStyle::Dot,
            name: String::new(),
            keys: Action::ALL.map(Action::default_key),
        }
    }
}

impl Settings {
    pub fn key(&self, action: Action) -> KeyCode {
        self.keys[action.index()]
    }

    /// Binds `key` to `action`. If another action already used that key, the two swap, so no
    /// action is ever left without a key.
    pub fn bind(&mut self, action: Action, key: KeyCode) {
        let previous = self.key(action);
        for other in Action::ALL {
            if other != action && self.key(other) == key {
                self.keys[other.index()] = previous;
            }
        }
        self.keys[action.index()] = key;
    }

    pub fn load() -> Self {
        fs::read_to_string(SETTINGS_FILE)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Err(error) = fs::write(SETTINGS_FILE, self.serialize()) {
            warn!("could not save {SETTINGS_FILE}: {error}");
        }
    }

    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "sensitivity" => {
                    if let Ok(v) = value.parse::<f32>() {
                        settings.sensitivity = v.clamp(SENSITIVITY_RANGE.0, SENSITIVITY_RANGE.1);
                    }
                }
                "fov" => {
                    if let Ok(v) = value.parse::<f32>() {
                        settings.fov = v.clamp(FOV_RANGE.0, FOV_RANGE.1);
                    }
                }
                "volume" => {
                    if let Ok(v) = value.parse::<f32>() {
                        settings.volume = v.clamp(0.0, 1.0);
                    }
                }
                "crosshair" => {
                    settings.crosshair = match value {
                        "cross" => CrosshairStyle::Cross,
                        "off" => CrosshairStyle::Off,
                        _ => CrosshairStyle::Dot,
                    };
                }
                "name" => settings.name = value.to_string(),
                _ => {
                    let action = key
                        .strip_prefix("key.")
                        .and_then(|name| Action::ALL.iter().find(|a| format!("{a:?}") == name));
                    let code = BINDABLE.iter().find(|k| format!("{k:?}") == value);
                    if let (Some(action), Some(code)) = (action, code) {
                        settings.bind(*action, *code);
                    }
                }
            }
        }
        settings
    }

    fn serialize(&self) -> String {
        let crosshair = match self.crosshair {
            CrosshairStyle::Dot => "dot",
            CrosshairStyle::Cross => "cross",
            CrosshairStyle::Off => "off",
        };
        let mut text = format!(
            "sensitivity={}\nfov={}\nvolume={}\ncrosshair={crosshair}\nname={}\n",
            self.sensitivity, self.fov, self.volume, self.name
        );
        for action in Action::ALL {
            text.push_str(&format!("key.{action:?}={:?}\n", self.key(action)));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_save_and_load() {
        let mut settings = Settings {
            sensitivity: 1.5,
            fov: 90.0,
            volume: 0.3,
            crosshair: CrosshairStyle::Cross,
            name: "Sniper".into(),
            ..default()
        };
        settings.bind(Action::Reload, KeyCode::KeyT);
        assert_eq!(Settings::parse(&settings.serialize()), settings);
    }

    #[test]
    fn rebinding_to_a_used_key_swaps_the_two_actions() {
        let mut settings = Settings::default();
        settings.bind(Action::Forward, KeyCode::KeyS);
        assert_eq!(settings.key(Action::Forward), KeyCode::KeyS);
        assert_eq!(settings.key(Action::Back), KeyCode::KeyW);
    }

    #[test]
    fn garbage_and_out_of_range_values_are_tamed() {
        let settings = Settings::parse("fov=500\nvolume=nope\nkey.Jump=NotAKey\n???\n");
        assert_eq!(settings.fov, FOV_RANGE.1);
        assert_eq!(settings.volume, Settings::default().volume);
        assert_eq!(settings.key(Action::Jump), KeyCode::Space);
    }
}
