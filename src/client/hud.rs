//! Everything drawn in 2D: health, ammo, crosshair, scope, hit marker, damage indicator,
//! kill feed, scoreboard, match banner and name tags above other players.


use super::*;
use crate::combat::{INTERMISSION_TICKS, MAGAZINE_SIZE, SCORE_LIMIT, trace};
use crate::player::eye_height;
use crate::protocol::{
    HealthUpdate, HitFeedback, KillFeed, MatchEvent, PlayerName, PlayerStats, ShotFx, TICK_RATE,
};
use crate::settings::{Action, CrosshairStyle};
use crate::world::LevelGeometry;

const KILL_FEED_SECONDS: f32 = 6.0;
const KILL_FEED_LINES: usize = 5;
const DAMAGE_FLASH_FADE: f32 = 1.6;

/// Top-level HUD entity (removed when we go back to the menu).
#[derive(Component)]
pub(super) struct HudEntity;

#[derive(Component)]
pub(super) struct HealthText;
#[derive(Component)]
pub(super) struct AmmoText;
#[derive(Component)]
pub(super) struct DeathText;
#[derive(Component)]
pub(super) struct HitMarkerText;
#[derive(Component)]
pub(super) struct ScopeOverlay;
#[derive(Component)]
pub(super) struct BreathText;
#[derive(Component)]
pub(super) struct CrosshairPart(CrosshairStyle);
/// 0 = top (hit from ahead), 1 = right, 2 = bottom (from behind), 3 = left.
#[derive(Component)]
pub(super) struct DamageEdge(usize);
#[derive(Component)]
pub(super) struct KillFeedText;
#[derive(Component)]
pub(super) struct ScoreboardRoot;
#[derive(Component)]
pub(super) struct ScoreColumn(usize);
#[derive(Component)]
pub(super) struct BannerText;
#[derive(Component)]
pub(super) struct NameTag(Entity);

/// Recent kills, newest last, with seconds left to show each.
#[derive(Resource, Default)]
pub(super) struct KillLog(Vec<(String, f32)>);

/// The winner screen between matches.
#[derive(Resource, Default)]
pub(super) struct MatchBanner {
    winner: Option<String>,
    seconds_left: f32,
}

/// Red edge flashes for where damage came from, indexed like `DamageEdge`.
#[derive(Resource, Default)]
pub(super) struct DamageFlash([f32; 4]);

/// Which screen edge to flash for a shot that came from `source`, given where we're facing:
/// 0 = ahead, 1 = right, 2 = behind, 3 = left.
fn damage_edge(eye: Vec3, yaw: f32, source: Vec3) -> usize {
    let offset = source - eye;
    let forward = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
    let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
    let (ahead, side) = (offset.dot(forward), offset.dot(right));
    if ahead.abs() >= side.abs() {
        if ahead >= 0.0 { 0 } else { 2 }
    } else if side >= 0.0 {
        1
    } else {
        3
    }
}

pub(super) fn reset_hud(
    mut commands: Commands,
    hud: Query<Entity, Or<(With<HudEntity>, With<NameTag>)>>,
    mut kill_log: ResMut<KillLog>,
    mut banner: ResMut<MatchBanner>,
    mut flash: ResMut<DamageFlash>,
) {
    for entity in &hud {
        commands.entity(entity).despawn();
    }
    *kill_log = KillLog::default();
    *banner = MatchBanner::default();
    *flash = DamageFlash::default();
}

fn text(content: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(content),
        TextFont {
            font_size: size,
            ..default()
        },
        TextColor(color),
    )
}

fn edge_node(index: usize) -> Node {
    let mut node = Node {
        position_type: PositionType::Absolute,
        ..default()
    };
    match index {
        0 => {
            node.left = Val::Px(0.0);
            node.top = Val::Px(0.0);
            node.width = Val::Percent(100.0);
            node.height = Val::Percent(9.0);
        }
        1 => {
            node.right = Val::Px(0.0);
            node.top = Val::Px(0.0);
            node.width = Val::Percent(6.0);
            node.height = Val::Percent(100.0);
        }
        2 => {
            node.left = Val::Px(0.0);
            node.bottom = Val::Px(0.0);
            node.width = Val::Percent(100.0);
            node.height = Val::Percent(9.0);
        }
        _ => {
            node.left = Val::Px(0.0);
            node.top = Val::Px(0.0);
            node.width = Val::Percent(6.0);
            node.height = Val::Percent(100.0);
        }
    }
    node
}

pub(super) fn spawn_hud(mut commands: Commands) {
    // Crosshair: a dot or a small cross, picked in the settings.
    commands.spawn((
        HudEntity,
        CrosshairPart(CrosshairStyle::Dot),
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
    for (width, height) in [(16.0, 2.0), (2.0, 16.0)] {
        commands.spawn((
            HudEntity,
            CrosshairPart(CrosshairStyle::Cross),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(width),
                height: Val::Px(height),
                margin: UiRect {
                    left: Val::Px(-width / 2.0),
                    top: Val::Px(-height / 2.0),
                    ..default()
                },
                ..default()
            },
            BackgroundColor(Color::WHITE),
        ));
    }

    // Scope: a round lens (the black outline blacks out everything outside the circle) with
    // thin crosshair lines through it.
    commands.spawn((
        HudEntity,
        ScopeOverlay,
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Vh(92.0),
            height: Val::Vh(92.0),
            margin: UiRect {
                left: Val::Vh(-46.0),
                top: Val::Vh(-46.0),
                ..default()
            },
            border_radius: BorderRadius::MAX,
            ..default()
        },
        Outline::new(Val::Vh(160.0), Val::ZERO, Color::BLACK),
    ));
    commands.spawn((
        HudEntity,
        ScopeOverlay,
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(4.0),
            top: Val::Percent(50.0),
            width: Val::Percent(92.0),
            height: Val::Px(1.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
    ));
    commands.spawn((
        HudEntity,
        ScopeOverlay,
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(4.0),
            width: Val::Px(1.0),
            height: Val::Percent(92.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
    ));
    commands.spawn((
        HudEntity,
        BreathText,
        text("", 20.0, Color::srgb(0.85, 0.9, 1.0)),
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(60.0),
            margin: UiRect::left(Val::Px(-110.0)),
            ..default()
        },
    ));

    // Damage direction indicators.
    for index in 0..4 {
        commands.spawn((
            HudEntity,
            DamageEdge(index),
            edge_node(index),
            BackgroundColor(Color::srgba(0.9, 0.05, 0.05, 0.0)),
        ));
    }

    // Your own health. Other players' health is never sent to you.
    commands.spawn((
        HudEntity,
        HealthText,
        text("HP 100", 32.0, Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(20.0),
            bottom: Val::Px(20.0),
            ..default()
        },
    ));
    commands.spawn((
        HudEntity,
        AmmoText,
        text("", 32.0, Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            right: Val::Px(20.0),
            bottom: Val::Px(20.0),
            ..default()
        },
    ));

    // Hit marker next to the crosshair.
    commands.spawn((
        HudEntity,
        HitMarkerText,
        text("X", 28.0, Color::WHITE),
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
        HudEntity,
        DeathText,
        text("YOU DIED - respawning...", 48.0, Color::srgb(0.9, 0.1, 0.1)),
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(30.0),
            margin: UiRect::left(Val::Px(-230.0)),
            ..default()
        },
    ));

    // Kill feed, top right.
    commands.spawn((
        HudEntity,
        KillFeedText,
        text("", 20.0, Color::WHITE),
        TextLayout::new_with_justify(Justify::Right),
        Node {
            position_type: PositionType::Absolute,
            right: Val::Px(20.0),
            top: Val::Px(20.0),
            ..default()
        },
    ));

    // Winner banner.
    commands.spawn((
        HudEntity,
        BannerText,
        text("", 44.0, Color::srgb(1.0, 0.85, 0.2)),
        TextLayout::new_with_justify(Justify::Center),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(8.0),
            width: Val::Px(700.0),
            margin: UiRect::left(Val::Px(-350.0)),
            ..default()
        },
    ));

    // Scoreboard (held key, or shown at the end of a match).
    commands
        .spawn((
            HudEntity,
            ScoreboardRoot,
            Visibility::Hidden,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(22.0),
                width: Val::Px(520.0),
                margin: UiRect::left(Val::Px(-260.0)),
                padding: UiRect::all(Val::Px(18.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.82)),
        ))
        .with_children(|root| {
            root.spawn(text(
                format!("SCOREBOARD   (first to {SCORE_LIMIT} kills wins)"),
                22.0,
                Color::srgb(0.65, 0.68, 0.74),
            ));
            root.spawn(Node {
                column_gap: Val::Px(20.0),
                ..default()
            })
            .with_children(|row| {
                for (column, width) in [(0, 320.0), (1, 80.0), (2, 80.0)] {
                    row.spawn((
                        ScoreColumn(column),
                        text("", 22.0, Color::WHITE),
                        Node {
                            width: Val::Px(width),
                            ..default()
                        },
                    ));
                }
            });
        });
}

/// Messages from the server: our health, hit confirmations, kills, matches, and everyone's shots.
#[allow(clippy::too_many_arguments)]
pub(super) fn receive_server_messages(
    prediction: Res<Prediction>,
    look: Res<LocalLook>,
    mut health: ResMut<LocalHealth>,
    mut hit_marker: ResMut<HitMarker>,
    mut flash: ResMut<DamageFlash>,
    mut kill_log: ResMut<KillLog>,
    mut banner: ResMut<MatchBanner>,
    mut shots: MessageWriter<ShotFired>,
    mut health_in: MessageReader<HealthUpdate>,
    mut hits_in: MessageReader<HitFeedback>,
    mut shots_in: MessageReader<ShotFx>,
    mut kills_in: MessageReader<KillFeed>,
    mut matches_in: MessageReader<MatchEvent>,
) {
    for update in health_in.read() {
        if update.health < health.0 {
            if let Some(source) = update.damage_source {
                let eye = prediction.state.map_or(source, |state| state.position);
                flash.0[damage_edge(eye, look.yaw, source)] = 1.0;
            }
        }
        health.0 = update.health;
    }
    for hit in hits_in.read() {
        hit_marker.timer = HIT_MARKER_SECONDS;
        hit_marker.headshot = hit.headshot;
        hit_marker.kill = hit.kill;
    }
    for shot in shots_in.read() {
        shots.write(ShotFired {
            origin: shot.origin,
            end: shot.end,
            local: false,
            hit_player: shot.hit_player,
        });
    }
    for kill in kills_in.read() {
        let suffix = if kill.headshot { "  (headshot)" } else { "" };
        kill_log.0.push((
            format!("{}  >  {}{suffix}", kill.killer, kill.victim),
            KILL_FEED_SECONDS,
        ));
    }
    for event in matches_in.read() {
        match event {
            MatchEvent::Won { winner } => {
                banner.winner = Some(winner.clone());
                banner.seconds_left = INTERMISSION_TICKS as f32 / TICK_RATE as f32;
            }
            MatchEvent::Started => banner.winner = None,
        }
    }
}

pub(super) fn update_health_text(
    health: Res<LocalHealth>,
    mut health_text: Single<(&mut Text, &mut TextColor), With<HealthText>>,
) {
    let (text, color) = &mut *health_text;
    text.0 = format!("HP {}", health.0.ceil() as i32);
    color.0 = if health.0 <= 30.0 {
        Color::srgb(0.9, 0.2, 0.2)
    } else {
        Color::WHITE
    };
}

pub(super) fn update_ammo_text(
    prediction: Res<Prediction>,
    mut ammo_text: Single<(&mut Text, &mut TextColor), With<AmmoText>>,
) {
    let (text, color) = &mut *ammo_text;
    let Some(state) = prediction.state else {
        text.0.clear();
        return;
    };
    if state.reload > 0 {
        let seconds = state.reload as f32 / TICK_RATE as f32;
        text.0 = format!("RELOADING {seconds:.1}s");
        color.0 = Color::srgb(1.0, 0.85, 0.2);
    } else {
        text.0 = format!("AMMO {}/{MAGAZINE_SIZE}", state.ammo);
        color.0 = if state.ammo <= 1 {
            Color::srgb(0.9, 0.2, 0.2)
        } else {
            Color::WHITE
        };
    }
}

pub(super) fn update_death_text(
    prediction: Res<Prediction>,
    mut death_text: Single<&mut Visibility, With<DeathText>>,
) {
    let alive = prediction.state.is_none_or(|s| s.alive);
    **death_text = if alive {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
}

pub(super) fn update_hit_marker(
    time: Res<Time>,
    mut hit_marker: ResMut<HitMarker>,
    mut marker: Single<(&mut Visibility, &mut TextColor), With<HitMarkerText>>,
) {
    hit_marker.timer = (hit_marker.timer - time.delta_secs()).max(0.0);
    let (visibility, color) = &mut *marker;
    **visibility = if hit_marker.timer > 0.0 {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    color.0 = if hit_marker.kill {
        Color::srgb(1.0, 0.1, 0.1)
    } else if hit_marker.headshot {
        Color::srgb(1.0, 0.85, 0.1)
    } else {
        Color::WHITE
    };
}

pub(super) fn update_scope(
    scoped: Res<Scoped>,
    sway: Res<ScopeSway>,
    settings: Res<Settings>,
    prediction: Res<Prediction>,
    mut lens: Query<&mut Visibility, (With<ScopeOverlay>, Without<BreathText>)>,
    mut breath: Single<(&mut Visibility, &mut Text), With<BreathText>>,
) {
    let alive = prediction.state.is_none_or(|s| s.alive);
    let show = scoped.0 && alive;
    let visibility = if show {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut lens_visibility in &mut lens {
        *lens_visibility = visibility;
    }

    let (breath_visibility, breath_text) = &mut *breath;
    **breath_visibility = visibility;
    let key = crate::settings::key_label(settings.key(Action::HoldBreath));
    breath_text.0 = if sway.winded > 0.0 {
        "OUT OF BREATH".to_string()
    } else {
        let filled = (sway.breath * 10.0).round() as usize;
        format!(
            "Breath [{}{}]  hold {key}",
            "|".repeat(filled),
            " ".repeat(10 - filled.min(10))
        )
    };
}

pub(super) fn update_crosshair(
    scoped: Res<Scoped>,
    settings: Res<Settings>,
    mut parts: Query<(&CrosshairPart, &mut Visibility)>,
) {
    for (part, mut visibility) in &mut parts {
        *visibility = if part.0 == settings.crosshair && !scoped.0 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

pub(super) fn update_damage_edges(
    time: Res<Time>,
    mut flash: ResMut<DamageFlash>,
    mut edges: Query<(&DamageEdge, &mut BackgroundColor)>,
) {
    for value in &mut flash.0 {
        *value = (*value - DAMAGE_FLASH_FADE * time.delta_secs()).max(0.0);
    }
    for (edge, mut color) in &mut edges {
        color.0 = Color::srgba(0.9, 0.05, 0.05, flash.0[edge.0] * 0.6);
    }
}

pub(super) fn update_banner(
    time: Res<Time>,
    mut banner: ResMut<MatchBanner>,
    mut text: Single<&mut Text, With<BannerText>>,
) {
    banner.seconds_left = (banner.seconds_left - time.delta_secs()).max(0.0);
    text.0 = match &banner.winner {
        Some(winner) => format!(
            "{winner} wins the match!\nNext match in {}s",
            banner.seconds_left.ceil() as i32
        ),
        None => String::new(),
    };
}

pub(super) fn update_kill_feed(
    time: Res<Time>,
    mut kill_log: ResMut<KillLog>,
    mut text: Single<&mut Text, With<KillFeedText>>,
) {
    for (_, seconds) in &mut kill_log.0 {
        *seconds -= time.delta_secs();
    }
    kill_log.0.retain(|(_, seconds)| *seconds > 0.0);
    let skip = kill_log.0.len().saturating_sub(KILL_FEED_LINES);
    text.0 = kill_log.0[skip..]
        .iter()
        .map(|(line, _)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
}

pub(super) fn update_scoreboard(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<Settings>,
    banner: Res<MatchBanner>,
    local_id: Option<Res<LocalClientId>>,
    players: Query<(&PlayerOwner, &PlayerName, &PlayerStats)>,
    mut root: Single<&mut Visibility, With<ScoreboardRoot>>,
    mut columns: Query<(&ScoreColumn, &mut Text)>,
) {
    let show = keys.pressed(settings.key(Action::Scoreboard)) || banner.winner.is_some();
    **root = if show {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if !show {
        return;
    }

    let mut rows: Vec<_> = players.iter().collect();
    rows.sort_by_key(|(_, name, stats)| {
        (std::cmp::Reverse(stats.kills), stats.deaths, name.0.clone())
    });

    let me = local_id.map(|id| id.0);
    let mut names = String::from("Player");
    let mut kills = String::from("Kills");
    let mut deaths = String::from("Deaths");
    for (owner, name, stats) in rows {
        let marker = if Some(owner.0) == me { "> " } else { "" };
        names.push_str(&format!("\n{marker}{}", name.0));
        kills.push_str(&format!("\n{}", stats.kills));
        deaths.push_str(&format!("\n{}", stats.deaths));
    }
    for (column, mut text) in &mut columns {
        text.0 = match column.0 {
            0 => names.clone(),
            1 => kills.clone(),
            _ => deaths.clone(),
        };
    }
}

pub(super) fn spawn_name_tag(commands: &mut Commands, player: Entity) {
    commands.spawn((
        NameTag(player),
        text("", 18.0, Color::srgb(0.95, 0.95, 0.8)),
        TextLayout::new_with_justify(Justify::Center),
        Visibility::Hidden,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Px(160.0),
            ..default()
        },
    ));
}

/// Floats each remote player's name above their head. Tags behind a wall are hidden, so a name
/// never gives away where someone is hiding.
pub(super) fn update_name_tags(
    mut commands: Commands,
    level: Res<LevelGeometry>,
    camera: Single<(&Camera, &Transform), With<ViewCamera>>,
    players: Query<
        (&PlayerState, &PlayerName, &Transform),
        (With<PlayerVisual>, Without<LocalPlayer>),
    >,
    mut tags: Query<(Entity, &NameTag, &mut Node, &mut Visibility, &mut Text)>,
) {
    let (camera, camera_transform) = *camera;
    let camera_global = GlobalTransform::from(*camera_transform);
    let eye = camera_transform.translation;

    for (tag_entity, tag, mut node, mut visibility, mut text) in &mut tags {
        let Ok((state, name, transform)) = players.get(tag.0) else {
            commands.entity(tag_entity).despawn();
            continue;
        };

        let above = transform.translation + Vec3::Y * (eye_height(state.crouched) + 0.55);
        let to_target = above - eye;
        let distance = to_target.length();
        let screen = camera.world_to_viewport(&camera_global, above).ok();
        let in_front = to_target.dot(camera_transform.forward().as_vec3()) > 0.0;
        let visible_to_us = state.alive
            && in_front
            && distance > 0.1
            && trace(eye, to_target / distance, &level.0, &[]).end_distance >= distance - 0.3;

        match screen {
            Some(position) if visible_to_us => {
                *visibility = Visibility::Inherited;
                node.left = Val::Px(position.x - 80.0);
                node.top = Val::Px(position.y - 22.0);
                if text.0 != name.0 {
                    text.0 = name.0.clone();
                }
            }
            _ => *visibility = Visibility::Hidden,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_edge_is_relative_to_where_we_face() {
        let eye = Vec3::ZERO;
        // Facing -Z (yaw 0): a shooter straight ahead, behind, to the right and to the left.
        assert_eq!(damage_edge(eye, 0.0, Vec3::new(0.0, 0.0, -10.0)), 0);
        assert_eq!(damage_edge(eye, 0.0, Vec3::new(10.0, 0.0, 0.0)), 1);
        assert_eq!(damage_edge(eye, 0.0, Vec3::new(0.0, 0.0, 10.0)), 2);
        assert_eq!(damage_edge(eye, 0.0, Vec3::new(-10.0, 0.0, 0.0)), 3);
        // Turned a quarter turn left (facing -X), a shooter at -X is now straight ahead.
        assert_eq!(damage_edge(eye, std::f32::consts::FRAC_PI_2, Vec3::new(-10.0, 0.0, 0.0)), 0);
    }
}
