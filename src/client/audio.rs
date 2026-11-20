//! Sound effects. There are no audio files: every sound is synthesised at startup from a few
//! sine waves and noise bursts, so the game has nothing to download or ship.
//!
//! Your own sounds are played flat; other players' shots and footsteps are positional, so you
//! can hear which way they are (the camera carries the listener).

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use bevy::audio::{AudioPlayer, AudioSource, GlobalVolume, PlaybackSettings, Volume};

use super::*;
use crate::protocol::{HealthUpdate, HitFeedback};

const SAMPLE_RATE: u32 = 44_100;

#[derive(Resource)]
pub(super) struct SoundBank {
    shot: Handle<AudioSource>,
    hit: Handle<AudioSource>,
    headshot: Handle<AudioSource>,
    kill: Handle<AudioSource>,
    hurt: Handle<AudioSource>,
    reload: Handle<AudioSource>,
    step: Handle<AudioSource>,
    slide: Handle<AudioSource>,
    respawn: Handle<AudioSource>,
}

/// Counts down to a remote player's next footstep sound.
#[derive(Component, Default)]
pub(super) struct StepTimer(f32);

/// Encodes mono samples (-1.0..=1.0) as a 16-bit WAV file.
fn wav(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// White noise in -1.0..=1.0.
fn noise(seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*seed >> 8) as f32 / (1 << 23) as f32 - 1.0
}

/// Builds `seconds` of audio by calling `voice(time, noise_seed)` for every sample.
fn synth(seconds: f32, mut voice: impl FnMut(f32, &mut u32) -> f32) -> Vec<f32> {
    let mut seed = 12_345;
    (0..(seconds * SAMPLE_RATE as f32) as usize)
        .map(|i| voice(i as f32 / SAMPLE_RATE as f32, &mut seed))
        .collect()
}

fn sine(frequency: f32, t: f32) -> f32 {
    (TAU * frequency * t).sin()
}

fn decay(rate: f32, t: f32) -> f32 {
    (-rate * t).exp()
}

fn rifle_body(t: f32, seed: &mut u32) -> f32 {
    noise(seed) * decay(22.0, t) * 0.9
        + sine(55.0, t) * decay(9.0, t) * 0.9
        + sine(140.0, t) * decay(30.0, t) * 0.5
}

fn make_sounds() -> Vec<(&'static str, Vec<f32>)> {
    // A sniper shot: crack, boom, and an echo off the distant walls.
    let shot = synth(1.1, |t, seed| {
        let echo = if t > 0.28 { rifle_body(t - 0.28, seed) * 0.3 } else { 0.0 };
        (rifle_body(t, seed) + echo) * 0.9
    });
    let hit = synth(0.1, |t, _| sine(1500.0, t) * decay(40.0, t) * 0.6);
    let headshot = synth(0.3, |t, _| {
        (sine(2200.0, t) + sine(3300.0, t) * 0.5) * decay(14.0, t) * 0.5
    });
    let kill = synth(0.4, |t, _| {
        (sine(520.0, t) + sine(780.0, t) * 0.7) * decay(9.0, t) * 0.5
    });
    let hurt = synth(0.3, |t, seed| {
        sine(90.0, t) * decay(14.0, t) * 0.8 + noise(seed) * decay(35.0, t) * 0.4
    });
    // Four mechanical clicks: magazine out, in, bolt back, bolt forward.
    let reload = synth(2.5, |t, seed| {
        let mut out = 0.0;
        for start in [0.2, 0.9, 1.6, 2.3] {
            if t >= start {
                let local = t - start;
                out += (noise(seed) * 0.6 + sine(900.0, local) * 0.4) * decay(70.0, local);
            }
        }
        out * 0.6
    });
    let mut low_pass = 0.0;
    let step = synth(0.12, |t, seed| {
        low_pass += (noise(seed) - low_pass) * 0.25;
        low_pass * decay(38.0, t) * 1.6
    });
    let mut swoosh = 0.0;
    let slide = synth(0.9, |t, seed| {
        swoosh += (noise(seed) - swoosh) * 0.12;
        swoosh * (PI * t / 0.9).sin().powi(2) * 1.4
    });
    let respawn = synth(0.4, |t, _| {
        (TAU * (300.0 * t + 625.0 * t * t)).sin() * (1.0 - t / 0.4) * 0.4
    });

    vec![
        ("shot", shot),
        ("hit", hit),
        ("headshot", headshot),
        ("kill", kill),
        ("hurt", hurt),
        ("reload", reload),
        ("step", step),
        ("slide", slide),
        ("respawn", respawn),
    ]
}

pub(super) fn setup_audio(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
    let mut sounds = make_sounds();
    let mut take = |name: &str| {
        let index = sounds.iter().position(|(n, _)| *n == name).unwrap_or(0);
        let (_, samples) = sounds.swap_remove(index);
        sources.add(AudioSource {
            bytes: Arc::from(wav(&samples)),
        })
    };
    commands.insert_resource(SoundBank {
        shot: take("shot"),
        hit: take("hit"),
        headshot: take("headshot"),
        kill: take("kill"),
        hurt: take("hurt"),
        reload: take("reload"),
        step: take("step"),
        slide: take("slide"),
        respawn: take("respawn"),
    });
}

pub(super) fn apply_volume(settings: Res<Settings>, mut global: ResMut<GlobalVolume>) {
    if settings.is_changed() {
        global.volume = Volume::Linear(settings.volume);
    }
}

fn play(commands: &mut Commands, sound: &Handle<AudioSource>, volume: f32) {
    commands.spawn((
        AudioPlayer::new(sound.clone()),
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume)),
    ));
}

fn play_at(commands: &mut Commands, sound: &Handle<AudioSource>, volume: f32, position: Vec3) {
    commands.spawn((
        AudioPlayer::new(sound.clone()),
        PlaybackSettings::DESPAWN
            .with_spatial(true)
            .with_volume(Volume::Linear(volume)),
        Transform::from_translation(position),
    ));
}

pub(super) fn play_shot_sounds(
    mut commands: Commands,
    bank: Res<SoundBank>,
    mut shots: MessageReader<ShotFired>,
) {
    for shot in shots.read() {
        if shot.local {
            play(&mut commands, &bank.shot, 0.9);
        } else {
            play_at(&mut commands, &bank.shot, 1.0, shot.origin);
        }
    }
}

/// Hit confirmations and getting hurt, straight from the server's word.
pub(super) fn play_feedback_sounds(
    mut commands: Commands,
    bank: Res<SoundBank>,
    mut hits: MessageReader<HitFeedback>,
    mut health: MessageReader<HealthUpdate>,
) {
    for hit in hits.read() {
        let sound = if hit.kill {
            &bank.kill
        } else if hit.headshot {
            &bank.headshot
        } else {
            &bank.hit
        };
        play(&mut commands, sound, 0.7);
    }
    for update in health.read() {
        if update.damage_source.is_some() {
            play(&mut commands, &bank.hurt, 0.9);
        }
    }
}

/// How often a walking player's feet make a sound; faster means quicker steps.
fn step_interval(speed: f32) -> f32 {
    (2.4 / speed).clamp(0.2, 0.6)
}

fn horizontal_speed(state: &PlayerState) -> f32 {
    Vec2::new(state.velocity.x, state.velocity.z).length()
}

/// Our own reload, slide, respawn and footstep sounds, from changes in the predicted state.
pub(super) fn play_state_sounds(
    mut commands: Commands,
    time: Res<Time>,
    bank: Res<SoundBank>,
    prediction: Res<Prediction>,
    mut previous: Local<Option<PlayerState>>,
    mut step_timer: Local<f32>,
) {
    let Some(state) = prediction.state else {
        *previous = None;
        return;
    };

    if let Some(before) = *previous {
        if before.reload == 0 && state.reload > 0 {
            play(&mut commands, &bank.reload, 0.7);
        }
        if before.slide_ticks == 0 && state.slide_ticks > 0 {
            play(&mut commands, &bank.slide, 0.6);
        }
        if !before.alive && state.alive {
            play(&mut commands, &bank.respawn, 0.6);
        }
    }
    *previous = Some(state);

    let speed = horizontal_speed(&state);
    *step_timer -= time.delta_secs();
    if state.alive && state.grounded && !state.crouched && speed > 2.5 {
        if *step_timer <= 0.0 {
            play(&mut commands, &bank.step, 0.35);
            *step_timer = step_interval(speed);
        }
    } else {
        *step_timer = 0.0;
    }
}

pub(super) fn play_remote_footsteps(
    mut commands: Commands,
    time: Res<Time>,
    bank: Res<SoundBank>,
    mut players: Query<
        (&PlayerState, &Transform, &mut StepTimer),
        (With<PlayerVisual>, Without<LocalPlayer>),
    >,
) {
    for (state, transform, mut timer) in &mut players {
        timer.0 -= time.delta_secs();
        let speed = horizontal_speed(state);
        if state.alive && state.grounded && !state.crouched && speed > 2.5 {
            if timer.0 <= 0.0 {
                play_at(&mut commands, &bank.step, 0.8, transform.translation);
                timer.0 = step_interval(speed);
            }
        } else {
            timer.0 = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_describes_the_samples() {
        let bytes = wav(&[0.0, 0.5, -0.5, 1.0]);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
    }
}
