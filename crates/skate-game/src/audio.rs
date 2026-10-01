//! Gameplay audio decoded from the owned disc.
//!
//! Two things are driven here, both gated the way the vehicle engine voice is
//! (`crate::graphics_menu::gameplay_active` and `!replay.active`):
//!
//! * a surface rolling loop whose grain follows the packed audio-surface id on the
//!   board's ground contacts and whose pitch/volume follow speed; and
//! * one-shot effects triggered by physical-state transitions (ollie, landing, bail,
//!   grind entry).
//!
//! The clips and their mapping live in `private/audio/audio.json`, produced by the
//! setup pipeline (`tools/asset_pipeline/audio.py`). Missing assets simply leave the
//! system disabled, so a copy set up without the optional decoder still runs.
use bevy::{
    audio::{PlaybackSettings, Volume},
    prelude::*,
};
use serde::Deserialize;
use std::collections::HashMap;

use crate::config::Config;
use crate::physics::{GamePhysics, SkaterRuntime};
use skate_core::player::state::PhysicalStateId;

const MANIFEST: &str = "private/audio/audio.json";
const ASSET_PREFIX: &str = "private/audio/";
const EFFECT_GAIN: f32 = 0.55;
const ROLLING_MIN_SPEED: f32 = 0.6;
const ROLLING_HARD_SPEED: f32 = 7.0;
const ROLLING_SPEED_RANGE: f32 = 9.0;
const ROLLING_MIN_GAIN: f32 = 0.30;
const ROLLING_MAX_GAIN: f32 = 0.42;

#[derive(Deserialize)]
struct Rolling {
    default: String,
    surfaces: HashMap<String, String>,
    files: HashMap<String, HashMap<String, String>>,
}

#[derive(Deserialize)]
struct Manifest {
    version: u32,
    rolling: Rolling,
    #[serde(default)]
    one_shots: HashMap<String, Vec<String>>,
}

/// Marker for the rolling-loop entity so its sink can be driven without touching
/// one-shot sinks.
#[derive(Component)]
struct RollingVoice;

struct Voice {
    entity: Entity,
    base: String,
    variant: String,
    volume: f32,
}

struct Fade {
    entity: Entity,
    volume: f32,
}

#[derive(Resource, Default)]
struct GameplayAudio {
    enabled: bool,
    rolling: HashMap<String, HashMap<String, Handle<AudioSource>>>,
    surfaces: HashMap<u32, String>,
    default: String,
    effects: HashMap<String, Vec<Handle<AudioSource>>>,
    voice: Option<Voice>,
    fades: Vec<Fade>,
    previous_state: Option<PhysicalStateId>,
}

pub(crate) struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameplayAudio>()
            .add_systems(Startup, load)
            .add_systems(Update, (update_rolling, update_effects));
    }
}

fn load(config: Res<Config>, assets: Res<AssetServer>, mut audio: ResMut<GameplayAudio>) {
    let path = config.asset_root.join(MANIFEST);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    let manifest: Manifest = match serde_json::from_slice(&bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            bevy::log::warn!("Gameplay audio manifest {path:?} is invalid: {error}");
            return;
        }
    };
    if manifest.version != 1 {
        bevy::log::warn!("Gameplay audio manifest has unsupported version {}", manifest.version);
        return;
    }

    for (base, variants) in &manifest.rolling.files {
        let mut handles = HashMap::new();
        for (variant, relative) in variants {
            let handle: Handle<AudioSource> = assets.load(format!("{ASSET_PREFIX}{relative}"));
            handles.insert(variant.clone(), handle);
        }
        audio.rolling.insert(base.clone(), handles);
    }
    for (id, base) in &manifest.rolling.surfaces {
        if let Ok(id) = id.parse::<u32>() {
            audio.surfaces.insert(id, base.clone());
        }
    }
    audio.default = manifest.rolling.default.clone();
    audio.default = if audio.rolling.contains_key(&audio.default) {
        audio.default.clone()
    } else {
        audio.rolling.keys().next().cloned().unwrap_or_default()
    };
    for (category, files) in &manifest.one_shots {
        audio
            .effects
            .insert(category.clone(), files.iter().map(|relative| assets.load(format!("{ASSET_PREFIX}{relative}"))).collect());
    }
    audio.enabled = !audio.rolling.is_empty();
}

fn rolling_surface(physics: &GamePhysics) -> Option<u32> {
    physics
        .board
        .contact_reports()
        .iter()
        .map(|report| u32::from(report.other_surface) & 0x7f)
        .find(|id| *id != 0)
}

fn update_rolling(
    mut commands: Commands,
    physics: Res<GamePhysics>,
    skater: Res<SkaterRuntime>,
    menu: Option<Res<crate::graphics_menu::Menu>>,
    replay: Res<crate::replay::Replay>,
    time: Res<Time<Real>>,
    mut audio: ResMut<GameplayAudio>,
    mut sinks: Query<&mut AudioSink, With<RollingVoice>>,
) {
    if !audio.enabled {
        return;
    }
    let active = crate::graphics_menu::gameplay_active(menu)
        && !replay.active
        && !physics.board_wiping_out
        && !skater.player_state.current().is_grind();
    let speed = physics.riding.motion.speed;
    let grounded = physics.riding.ground.wheel_contact_count > 0;
    let rolling = active && grounded && speed > ROLLING_MIN_SPEED;

    let variant = if speed > ROLLING_HARD_SPEED { "hard" } else { "soft" };
    let base = if rolling {
        rolling_surface(&physics)
            .and_then(|id| audio.surfaces.get(&id).cloned())
            .filter(|base| audio.rolling.contains_key(base))
            .unwrap_or_else(|| audio.default.clone())
    } else {
        String::new()
    };

    if rolling {
        let stale = audio
            .voice
            .as_ref()
            .is_none_or(|voice| voice.base != base || voice.variant != variant);
        if stale {
            if let Some(handle) = audio.rolling.get(&base).and_then(|map| map.get(variant)).cloned() {
                if let Some(previous) = audio.voice.take() {
                    audio.fades.push(Fade { entity: previous.entity, volume: previous.volume });
                }
                let entity = commands
                    .spawn((
                        RollingVoice,
                        AudioPlayer(handle),
                        PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)),
                    ))
                    .id();
                audio.voice = Some(Voice { entity, base, variant: variant.to_string(), volume: 0.0 });
            }
        }
    } else if let Some(previous) = audio.voice.take() {
        audio.fades.push(Fade { entity: previous.entity, volume: previous.volume });
    }

    let dt = time.delta_secs().min(0.1);
    // A gentle rise with speed. The earlier 0.10..0.50 span was ~12 dB, which
    // stacked on the rolling clips' own level and made a steady roll swell into
    // something overpowering within a few seconds; 0.30..0.42 keeps the speed
    // cue audible without the runaway.
    let target_volume = if rolling {
        (0.30 + 0.12 * (speed / ROLLING_SPEED_RANGE)).clamp(ROLLING_MIN_GAIN, ROLLING_MAX_GAIN)
    } else {
        0.0
    };
    let target_pitch = (0.85 + speed / 20.0).clamp(0.7, 1.5);

    if let Some(voice) = &mut audio.voice {
        voice.volume += (target_volume - voice.volume) * (1.0 - (-10.0 * dt).exp());
        if let Ok(mut sink) = sinks.get_mut(voice.entity) {
            sink.set_volume(Volume::Linear(voice.volume));
            sink.set_speed(target_pitch);
        }
    }
    for fade in &mut audio.fades {
        fade.volume += (0.0 - fade.volume) * (1.0 - (-10.0 * dt).exp());
        if let Ok(mut sink) = sinks.get_mut(fade.entity) {
            sink.set_volume(Volume::Linear(fade.volume.max(0.0)));
        }
    }
    audio.fades.retain(|fade| {
        if fade.volume < 0.002 {
            commands.entity(fade.entity).despawn();
            false
        } else {
            true
        }
    });
}

fn is_air(state: PhysicalStateId) -> bool {
    matches!(
        state,
        PhysicalStateId::PhysicsAir
            | PhysicalStateId::KnownAir
            | PhysicalStateId::PhysicsAirSecondary
            | PhysicalStateId::BipedAir
            | PhysicalStateId::HandPlant
            | PhysicalStateId::FootPlant
            | PhysicalStateId::Boneless
    )
}

fn is_ground(state: PhysicalStateId) -> bool {
    !is_air(state)
        && !state.is_grind()
        && !matches!(
            state,
            PhysicalStateId::WipeoutGround
                | PhysicalStateId::Sleeping
                | PhysicalStateId::Nonspecific
                | PhysicalStateId::Teleporting
        )
}

fn update_effects(
    mut commands: Commands,
    physics: Res<GamePhysics>,
    skater: Res<SkaterRuntime>,
    menu: Option<Res<crate::graphics_menu::Menu>>,
    replay: Res<crate::replay::Replay>,
    mut audio: ResMut<GameplayAudio>,
) {
    if !audio.enabled {
        return;
    }
    let current = skater.player_state.current();
    let previous = audio.previous_state.replace(current);
    let Some(previous) = previous else {
        return;
    };
    if !crate::graphics_menu::gameplay_active(menu) || replay.active {
        return;
    }

    let category = if current == PhysicalStateId::WipeoutGround && previous != PhysicalStateId::WipeoutGround {
        "bail"
    } else if is_ground(current) && is_air(previous) {
        "land"
    } else if is_air(current) && is_ground(previous) {
        "pop"
    } else if current.is_grind() && !previous.is_grind() {
        "grind"
    } else {
        return;
    };

    let Some(handles) = audio.effects.get(category) else {
        return;
    };
    let Some(handle) = handles
        .get((physics.ticks as usize) % handles.len().max(1))
        .cloned()
    else {
        return;
    };
    commands.spawn((
        AudioPlayer(handle),
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(EFFECT_GAIN)),
    ));
}
