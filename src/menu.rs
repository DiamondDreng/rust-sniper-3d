//! Main menu: pick a saved server (or type an address) and connect.
//!
//! Saved servers live in `servers.txt` next to where you run the game, one per line as
//! `name|address`. Lines starting with `#` are ignored, so you can also edit it by hand.
//!
//! The menu is rebuilt from `MenuState` whenever something changes. That's not the fastest way
//! to do UI, but a menu this size doesn't need anything fancier.

use std::fs;
use std::net::{SocketAddr, ToSocketAddrs};

use bevy::app::AppExit;
use bevy::ecs::spawn::SpawnIter;
use bevy::input::{ButtonState, keyboard::KeyboardInput};
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_replicon_renet::{RenetClient, netcode::NetcodeClientTransport};

use crate::client::begin_connect;
use crate::protocol::DEFAULT_PORT;

const SERVERS_FILE: &str = "servers.txt";
const MAX_INPUT_LEN: usize = 80;

const WHITE: Color = Color::WHITE;
const GRAY: Color = Color::srgb(0.65, 0.68, 0.74);
const ERROR_RED: Color = Color::srgb(0.95, 0.38, 0.38);
const INFO_BLUE: Color = Color::srgb(0.65, 0.85, 1.0);
const PANEL: Color = Color::srgb(0.08, 0.09, 0.12);
const ROW: Color = Color::srgb(0.14, 0.15, 0.20);
const ROW_SELECTED: Color = Color::srgb(0.20, 0.36, 0.62);
const FIELD: Color = Color::srgb(0.12, 0.13, 0.17);
const FIELD_FOCUSED: Color = Color::srgb(0.20, 0.24, 0.34);
const BUTTON_BLUE: Color = Color::srgb(0.18, 0.34, 0.58);
const BUTTON_RED: Color = Color::srgb(0.55, 0.18, 0.18);
const BUTTON_GRAY: Color = Color::srgb(0.25, 0.27, 0.32);

/// Which screen the client is on. Gameplay systems only run in `Playing`.
#[derive(States, Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppScreen {
    #[default]
    Menu,
    Playing,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkPhase {
    #[default]
    Idle,
    Connecting,
    Connected,
}

/// Our own view of the current connection attempt, used to explain what happened
/// (e.g. "could not connect" vs "you chose to disconnect").
#[derive(Resource, Default)]
pub struct Link {
    pub target: Option<SocketAddr>,
    pub phase: LinkPhase,
    /// Set when the player pressed Cancel/Disconnect, so we don't report it as an error.
    pub user_requested: bool,
}

/// A message shown at the top of the menu.
pub struct Notice {
    pub text: String,
    pub error: bool,
}

#[derive(Clone)]
struct SavedServer {
    name: String,
    address: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Address,
}

#[derive(Resource)]
pub struct MenuState {
    servers: Vec<SavedServer>,
    selected: Option<usize>,
    name_input: String,
    address_input: String,
    focus: Field,
    pub status: Option<Notice>,
    /// The menu is rebuilt next frame when this is set.
    pub dirty: bool,
}

impl MenuState {
    fn new() -> Self {
        let servers = load_servers();
        Self {
            selected: if servers.is_empty() { None } else { Some(0) },
            servers,
            name_input: String::new(),
            address_input: String::new(),
            focus: Field::Address,
            status: None,
            dirty: true,
        }
    }
}

#[derive(Component, Clone)]
enum MenuAction {
    Select(usize),
    Focus(Field),
    Connect,
    Delete,
    Join,
    Save,
    Cancel,
    Quit,
}

#[derive(Message)]
struct DoAction(MenuAction);

#[derive(Component)]
struct MenuUi;

/// The resting color of a button; hover and press effects are computed from it.
#[derive(Component)]
struct BaseColor(Color);

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AppScreen>()
            .init_resource::<Link>()
            .insert_resource(MenuState::new())
            .add_message::<DoAction>()
            .add_systems(OnEnter(AppScreen::Menu), mark_dirty)
            .add_systems(OnExit(AppScreen::Menu), close_menu)
            .add_systems(
                Update,
                (
                    type_into_fields,
                    click_buttons,
                    run_actions,
                    rebuild_menu,
                    hover_colors,
                )
                    .chain()
                    .run_if(in_state(AppScreen::Menu)),
            );
    }
}

// ---------------------------------------------------------------------------------------------
// Saved servers
// ---------------------------------------------------------------------------------------------

fn load_servers() -> Vec<SavedServer> {
    let Ok(text) = fs::read_to_string(SERVERS_FILE) else {
        return vec![SavedServer {
            name: "Localhost".into(),
            address: format!("127.0.0.1:{DEFAULT_PORT}"),
        }];
    };

    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| match line.split_once('|') {
            Some((name, address)) => SavedServer {
                name: name.trim().to_string(),
                address: address.trim().to_string(),
            },
            None => SavedServer {
                name: line.to_string(),
                address: line.to_string(),
            },
        })
        .collect()
}

fn save_servers(servers: &[SavedServer]) {
    let text: String = servers
        .iter()
        .map(|server| format!("{}|{}\n", server.name.replace('|', " "), server.address))
        .collect();
    if let Err(error) = fs::write(SERVERS_FILE, text) {
        warn!("could not save {SERVERS_FILE}: {error}");
    }
}

/// Turns "host" or "host:port" into an IPv4 socket address (the default port is used if missing).
fn resolve_address(input: &str) -> Result<SocketAddr, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Type a server address first.".into());
    }

    let (host, port) = match input.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| format!("'{port}' is not a valid port number."))?;
            (host, port)
        }
        None => (input, DEFAULT_PORT),
    };

    (host, port)
        .to_socket_addrs()
        .map_err(|_| format!("Could not look up '{host}'."))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| format!("'{host}' has no IPv4 address."))
}

fn try_connect(
    address: &str,
    commands: &mut Commands,
    channels: &RepliconChannels,
    link: &mut Link,
    menu: &mut MenuState,
) {
    menu.status = Some(match resolve_address(address) {
        Ok(addr) => match begin_connect(commands, channels, link, addr) {
            Ok(()) => Notice {
                text: format!("Connecting to {addr}..."),
                error: false,
            },
            Err(error) => Notice {
                text: format!("Could not start the connection: {error}"),
                error: true,
            },
        },
        Err(message) => Notice {
            text: message,
            error: true,
        },
    });
}

// ---------------------------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------------------------

fn type_into_fields(
    mut keys: MessageReader<KeyboardInput>,
    mut menu: ResMut<MenuState>,
    link: Res<Link>,
    mut actions: MessageWriter<DoAction>,
) {
    for event in keys.read() {
        if event.state != ButtonState::Pressed || link.phase != LinkPhase::Idle {
            continue;
        }

        let menu = &mut *menu;
        match event.key_code {
            KeyCode::Backspace => {
                match menu.focus {
                    Field::Name => menu.name_input.pop(),
                    Field::Address => menu.address_input.pop(),
                };
                menu.dirty = true;
            }
            KeyCode::Tab => {
                menu.focus = match menu.focus {
                    Field::Name => Field::Address,
                    Field::Address => Field::Name,
                };
                menu.dirty = true;
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                // Typed an address? Join it. Otherwise connect to the selected server.
                let action = if menu.address_input.trim().is_empty() {
                    MenuAction::Connect
                } else {
                    MenuAction::Join
                };
                actions.write(DoAction(action));
            }
            _ => {
                if let Some(text) = &event.text {
                    let field = match menu.focus {
                        Field::Name => &mut menu.name_input,
                        Field::Address => &mut menu.address_input,
                    };
                    for character in text.chars().filter(|c| !c.is_control()) {
                        if field.chars().count() < MAX_INPUT_LEN {
                            field.push(character);
                            menu.dirty = true;
                        }
                    }
                }
            }
        }
    }
}

fn click_buttons(
    buttons: Query<(&Interaction, &MenuAction), (Changed<Interaction>, With<Button>)>,
    mut actions: MessageWriter<DoAction>,
) {
    for (interaction, action) in &buttons {
        if *interaction == Interaction::Pressed {
            actions.write(DoAction(action.clone()));
        }
    }
}

fn run_actions(
    mut actions: MessageReader<DoAction>,
    mut commands: Commands,
    mut menu: ResMut<MenuState>,
    mut link: ResMut<Link>,
    channels: Res<RepliconChannels>,
    mut exit: MessageWriter<AppExit>,
    mut transport: Option<ResMut<NetcodeClientTransport>>,
) {
    for DoAction(action) in actions.read() {
        menu.dirty = true;
        let idle = link.phase == LinkPhase::Idle;

        match action {
            MenuAction::Quit => {
                exit.write(AppExit::Success);
            }
            MenuAction::Cancel => {
                // Tell the server we're leaving (if we got that far), then drop the connection.
                link.user_requested = true;
                if let Some(transport) = transport.as_mut() {
                    transport.disconnect();
                }
                commands.remove_resource::<RenetClient>();
                commands.remove_resource::<NetcodeClientTransport>();
            }
            _ if !idle => {}
            MenuAction::Select(index) => menu.selected = Some(*index),
            MenuAction::Focus(field) => menu.focus = *field,
            MenuAction::Connect => {
                let address = menu
                    .selected
                    .and_then(|index| menu.servers.get(index))
                    .map(|server| server.address.clone());
                match address {
                    Some(address) => {
                        try_connect(&address, &mut commands, &channels, &mut link, &mut menu)
                    }
                    None => {
                        menu.status = Some(Notice {
                            text: "Select a server from the list first.".into(),
                            error: true,
                        })
                    }
                }
            }
            MenuAction::Join => {
                let address = menu.address_input.clone();
                try_connect(&address, &mut commands, &channels, &mut link, &mut menu);
            }
            MenuAction::Save => {
                let address = menu.address_input.trim().to_string();
                match resolve_address(&address) {
                    Ok(_) => {
                        let name = menu.name_input.trim();
                        let name = if name.is_empty() {
                            address.clone()
                        } else {
                            name.to_string()
                        };
                        menu.servers.push(SavedServer { name, address });
                        save_servers(&menu.servers);
                        menu.selected = Some(menu.servers.len() - 1);
                        menu.name_input.clear();
                        menu.address_input.clear();
                        menu.status = Some(Notice {
                            text: "Server saved.".into(),
                            error: false,
                        });
                    }
                    Err(message) => {
                        menu.status = Some(Notice {
                            text: message,
                            error: true,
                        })
                    }
                }
            }
            MenuAction::Delete => {
                if let Some(index) = menu.selected {
                    if index < menu.servers.len() {
                        menu.servers.remove(index);
                        save_servers(&menu.servers);
                        menu.selected = match menu.servers.len() {
                            0 => None,
                            len => Some(index.min(len - 1)),
                        };
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------------------------

fn label(text: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(text.into()),
        TextFont {
            font_size: size,
            ..default()
        },
        TextColor(color),
    )
}

fn button(text: &'static str, action: MenuAction, color: Color, width: f32) -> impl Bundle {
    (
        Button,
        Node {
            width: Val::Px(width),
            height: Val::Px(42.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(color),
        BaseColor(color),
        action,
        children![label(text, 20.0, WHITE)],
    )
}

fn server_row(index: usize, server: SavedServer, selected: bool) -> impl Bundle {
    let color = if selected { ROW_SELECTED } else { ROW };
    (
        Button,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(9.0)),
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        },
        BackgroundColor(color),
        BaseColor(color),
        MenuAction::Select(index),
        children![
            label(server.name, 20.0, WHITE),
            label(server.address, 18.0, GRAY),
        ],
    )
}

fn text_field(
    placeholder: &'static str,
    value: String,
    focused: bool,
    which: Field,
) -> impl Bundle {
    let empty = value.is_empty() && !focused;
    let shown = if empty {
        placeholder.to_string()
    } else if focused {
        format!("{value}_")
    } else {
        value.to_string()
    };
    let color = if focused { FIELD_FOCUSED } else { FIELD };
    (
        Button,
        Node {
            width: Val::Percent(100.0),
            height: Val::Px(40.0),
            padding: UiRect::horizontal(Val::Px(12.0)),
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(color),
        BaseColor(color),
        MenuAction::Focus(which),
        children![label(shown, 19.0, if empty { GRAY } else { WHITE })],
    )
}

fn panel() -> impl Bundle {
    (
        Node {
            flex_direction: FlexDirection::Column,
            width: Val::Px(640.0),
            row_gap: Val::Px(12.0),
            padding: UiRect::all(Val::Px(28.0)),
            ..default()
        },
        BackgroundColor(PANEL),
    )
}

fn button_row() -> Node {
    Node {
        column_gap: Val::Px(10.0),
        ..default()
    }
}

fn mark_dirty(mut menu: ResMut<MenuState>) {
    menu.dirty = true;
}

fn close_menu(mut commands: Commands, menu_ui: Query<Entity, With<MenuUi>>) {
    for entity in &menu_ui {
        commands.entity(entity).despawn();
    }
}

fn rebuild_menu(
    mut commands: Commands,
    mut menu: ResMut<MenuState>,
    link: Res<Link>,
    existing: Query<Entity, With<MenuUi>>,
) {
    if !(menu.dirty || link.is_changed()) {
        return;
    }
    menu.dirty = false;

    for entity in &existing {
        commands.entity(entity).despawn();
    }

    let panel_entity = if link.phase != LinkPhase::Idle {
        let target = link.target.map(|a| a.to_string()).unwrap_or_default();
        commands
            .spawn((
                panel(),
                children![
                    label("RUSTSHOOTER", 44.0, WHITE),
                    label(format!("Connecting to {target}..."), 22.0, INFO_BLUE),
                    button("Cancel", MenuAction::Cancel, BUTTON_RED, 160.0),
                ],
            ))
            .id()
    } else {
        let rows: Vec<_> = menu
            .servers
            .iter()
            .enumerate()
            .map(|(index, server)| server_row(index, server.clone(), menu.selected == Some(index)))
            .collect();

        let (status_text, status_color) = match &menu.status {
            Some(notice) => (
                notice.text.clone(),
                if notice.error { ERROR_RED } else { INFO_BLUE },
            ),
            None => (String::new(), GRAY),
        };

        commands
            .spawn((
                panel(),
                children![
                    label("RUSTSHOOTER", 44.0, WHITE),
                    label(status_text, 18.0, status_color),
                    label("Servers", 22.0, GRAY),
                    (
                        Node {
                            flex_direction: FlexDirection::Column,
                            width: Val::Percent(100.0),
                            row_gap: Val::Px(6.0),
                            ..default()
                        },
                        Children::spawn(SpawnIter(rows.into_iter())),
                    ),
                    (
                        button_row(),
                        children![
                            button("Connect", MenuAction::Connect, BUTTON_BLUE, 150.0),
                            button("Delete", MenuAction::Delete, BUTTON_RED, 150.0),
                        ],
                    ),
                    label("Add a server, or join an address directly", 22.0, GRAY),
                    text_field(
                        "Name (optional)",
                        menu.name_input.clone(),
                        menu.focus == Field::Name,
                        Field::Name,
                    ),
                    text_field(
                        "Address, e.g. 203.0.113.5 or 203.0.113.5:5000",
                        menu.address_input.clone(),
                        menu.focus == Field::Address,
                        Field::Address,
                    ),
                    (
                        button_row(),
                        children![
                            button("Join address", MenuAction::Join, BUTTON_BLUE, 190.0),
                            button("Save to list", MenuAction::Save, BUTTON_GRAY, 190.0),
                        ],
                    ),
                    button("Quit", MenuAction::Quit, BUTTON_GRAY, 150.0),
                ],
            ))
            .id()
    };

    let root = commands
        .spawn((
            MenuUi,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.78)),
        ))
        .id();
    commands.entity(root).add_child(panel_entity);
}

fn lighten(color: Color, amount: f32) -> Color {
    let c = color.to_srgba();
    Color::srgba(
        (c.red + amount).min(1.0),
        (c.green + amount).min(1.0),
        (c.blue + amount).min(1.0),
        c.alpha,
    )
}

fn hover_colors(
    mut buttons: Query<
        (&Interaction, &BaseColor, &mut BackgroundColor),
        (Changed<Interaction>, With<Button>),
    >,
) {
    for (interaction, base, mut background) in &mut buttons {
        background.0 = match interaction {
            Interaction::Pressed => lighten(base.0, 0.14),
            Interaction::Hovered => lighten(base.0, 0.07),
            Interaction::None => base.0,
        };
    }
}
