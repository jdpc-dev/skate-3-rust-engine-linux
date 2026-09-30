mod frame_timing;
mod animation;
mod crash_report;
mod crash_context;
mod multiplayer;
mod apt_vm;
mod apt_display;
mod apt_movie;
mod apt_text;
mod apt_scene;
mod apt_imports;
mod hud_runtime;
mod scoring_runtime;
mod scoring_hud;
mod animation_pose;
mod app;
mod assets;
mod audio;
mod camera;
mod config;
mod setup;
mod updater;
mod map_library;
mod map_render;
mod map_transition;
mod difficulty;
mod custom_difficulty;
mod graph_host;
mod graph_runtime;
mod input;
mod session_marker;
mod physics;
mod skater_animation;
mod verification;
mod performance;
mod profiling;
mod graphics_menu;
mod retail_menus;
mod retail_menu_movies;
mod modding;
mod customiser;
mod customiser_parts;
mod customiser_material;
mod custom_models;
mod teleport_menu;
mod render_capacity;
mod retail_render;
mod retail_character;
mod retail_exposure;
mod retail_irradiance;
mod retail_sky;
mod presentation;
mod debug_cam;
mod replay;
mod world;
mod grind_world;
mod skate_world;
mod map_validation;
mod water_splash;
mod game_audio;
pub(crate) mod world_audio;
pub(crate) mod ui_audio;
mod trigger_volumes;

fn main() -> bevy::app::AppExit {
    match updater::recover() {
        Ok(true) => return bevy::app::AppExit::Success,
        Err(error) => { eprintln!("{error}"); return bevy::app::AppExit::Success; },
        Ok(false) => {}
    }
    // Setup tool mode: `--extract-ocean-pca <default.xex> <ocean-pca.json>`.
    // Before the crash supervisor, whose report window would block setup on
    // a failed (non-zero) extraction.
    if let Some(code) = extract_ocean_pca() { std::process::exit(code); }
    // Setup tool mode: `--extract-menu-tables <default.xex> <menu-tables.json>`.
    if let Some(code) = extract_menu_tables() { std::process::exit(code); }
    if let Some(code) = crash_report::entry() { std::process::exit(code); }
    let _trace = match profiling::init() {
        Ok(guard) => guard,
        Err(error) => { eprintln!("{error}"); return bevy::app::AppExit::error(); }
    };
    let _startup = bevy::log::info_span!("startup").entered();
    eprintln!("REPORT_META stage=configuration_and_installation");
    let config = match bevy::log::info_span!("load_configuration_and_map").in_scope(config::Config::from_env) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    profiling::map_metadata(&config);
    eprintln!("REPORT_META startup=map_fingerprint:{:016x} difficulty:{} multiplayer_requested:{} custom_appearance:{} renderer:Vulkan", config.map_fingerprint, config.difficulty.key(), config.multiplayer.host.is_some() || config.multiplayer.direct.is_some(), config.multiplayer.appearance.is_some());
    eprintln!("REPORT_META stage=gameplay_configuration");
    if let Err(error) = skate_data::input_config::StockGameplayConfig::load(&config.asset_root) {
        eprintln!("{error}");
        return bevy::app::AppExit::error();
    }
    eprintln!("REPORT_META stage=asset_manifest");
    let manifest = match bevy::log::info_span!("load_manifest").in_scope(|| skate_data::GameAssets::load(&config.asset_root)) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    eprintln!("REPORT_META stage=stock_graphs");
    let graphs = match bevy::log::info_span!("load_graphs").in_scope(|| graph_runtime::StockGraphs::load(&config.asset_root, &manifest)) {
        Ok(graphs) => graphs,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    if config.validate_maps {
        return match map_validation::run(&config, &graphs) {
            Ok(()) => bevy::app::AppExit::Success,
            Err(error) => {
                eprintln!("{error}");
                bevy::app::AppExit::error()
            }
        };
    }
    eprintln!("REPORT_META stage=map_validation");
    if let Some(map) = &config.map {
        if let Err(error) = skate_world::validate_runtime(map) {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    }
    eprintln!("SKATE_DIFFICULTY mode={} native_index={}", config.difficulty.key(), config.difficulty as u32);
    eprintln!("REPORT_META stage=physics_initialization");
    let mut physics = match bevy::log::info_span!("load_physics").in_scope(|| physics::GamePhysics::load_with_difficulty(&config.asset_root, config.map.as_ref(), config.difficulty)) {
        Ok(physics) => physics,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    if config.multiplayer.spawn_offset != 0.0 {
        let mut spawn = physics.board.part_transforms()[skate_core::physics::board::BodyId::Deck.index()];
        spawn.translation.x += config.multiplayer.spawn_offset;
        physics.board.set_transform(spawn);
    }
    eprintln!("REPORT_META stage=skater_initialization");
    let skater = match bevy::log::info_span!("load_skater").in_scope(|| physics::SkaterRuntime::load(&config.asset_root, &graphs, &physics, config.difficulty.profile_key())) {
        Ok(skater) => skater,
        Err(error) => {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
    };
    eprintln!("REPORT_META stage=controls_initialization");
    let controls = match physics::PlayerControls::load(&config.asset_root) {
        Ok(controls) => controls,
        Err(error) => { eprintln!("{error}"); return bevy::app::AppExit::error(); }
    };
    if config.check_assets {
        if let Err(error) = camera::CameraRuntime::load(&config.asset_root) {
            eprintln!("{error}");
            return bevy::app::AppExit::error();
        }
        match trigger_volumes::check(config.map_path.as_deref(), config.map.as_ref()) {
            Ok(count) => eprintln!("SKATE_TRIGGERS_READY volumes={count}"),
            Err(error) => {
                eprintln!("{error}");
                return bevy::app::AppExit::error();
            }
        }
        eprintln!("SKATE_ASSETS_READY");
        return bevy::app::AppExit::Success;
    }
    eprintln!("REPORT_META stage=renderer_and_app_initialization");
    let mut app = app::build(config, manifest, graphs, physics, skater);
    app.insert_resource(controls);
    eprintln!("REPORT_META stage=app_run");
    drop(_startup);
    app.run()
}

#[cfg(test)]
#[path = "tests/action_host.rs"]
mod action_host_tests;

/// Writes the retail water/ocean animation table for setup (see
/// skate_data::ocean_pca). Runs before any game initialization.
fn extract_ocean_pca() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != "--extract-ocean-pca" {
        return None;
    }
    let (Some(xex), Some(out), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("Usage: skate3rust --extract-ocean-pca <default.xex> <ocean-pca.json>");
        return Some(2);
    };
    let result = std::fs::read(&xex)
        .map_err(|e| format!("{}: {e}", std::path::Path::new(&xex).display()))
        .and_then(|bytes| skate_data::ocean_pca::json_from_xex(&bytes))
        .and_then(|json| std::fs::write(&out, json).map_err(|e| format!("{}: {e}", std::path::Path::new(&out).display())));
    match result {
        Ok(()) => {
            println!("OCEAN_PCA_READY");
            Some(0)
        }
        Err(error) => {
            eprintln!("Ocean PCA extraction failed: {error}");
            Some(1)
        }
    }
}

/// Writes the retail front-end menu tables for setup (see
/// skate_data::menu_tables). Runs before any game initialization.
fn extract_menu_tables() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != "--extract-menu-tables" {
        return None;
    }
    let (Some(xex), Some(out), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("Usage: skate3rust --extract-menu-tables <default.xex> <menu-tables.json>");
        return Some(2);
    };
    let result = std::fs::read(&xex)
        .map_err(|e| format!("{}: {e}", std::path::Path::new(&xex).display()))
        .and_then(|bytes| skate_data::menu_tables::json_from_xex(&bytes))
        .and_then(|json| std::fs::write(&out, json).map_err(|e| format!("{}: {e}", std::path::Path::new(&out).display())));
    match result {
        Ok(()) => {
            println!("MENU_TABLES_READY");
            Some(0)
        }
        Err(error) => {
            eprintln!("Menu table extraction failed: {error}");
            Some(1)
        }
    }
}
