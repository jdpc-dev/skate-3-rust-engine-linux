//! Native-resolution pause UI over a separately scaled 3D render target.
use crate::difficulty::Difficulty;
use bevy::{
    camera::RenderTarget,
    core_pipeline::prepass::DepthPrepass,
    image::ImageSampler,
    prelude::*,
    render::{
        experimental::occlusion_culling::OcclusionCulling,
        render_resource::{Extent3d, TextureFormat},
        renderer::RenderAdapter,
    },
    window::{PresentMode, PrimaryWindow},
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const RESOLUTIONS: &[(u32, u32)] = &[
    (1280, 720),
    (1280, 800),
    (1600, 900),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];
const SCALES: &[u32] = &[25, 50, 67, 75, 85, 100];
const DAY_SPEEDS: &[u32] = &[0, 1, 10, 30, 60, 120, 360, 720];
const LIMITS: &[u32] = &[0, 30, 60, 90, 120, 144, 165, 240];
const SHADOW_QUALITIES: &[ShadowQuality] = &[
    ShadowQuality::Default,
    ShadowQuality::Reduced,
    ShadowQuality::Off,
];
const TEXTURE_DETAILS: &[u32] = &[100, 50];
const DRAW_DISTANCES: &[DrawDistance] = &[
    DrawDistance::Full,
    DrawDistance::Medium,
    DrawDistance::Short,
];
/// Camera distance past which a static batch entirely out of range is hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DrawDistance {
    Full,
    Medium,
    Short,
}
impl Default for DrawDistance {
    fn default() -> Self {
        Self::Full
    }
}
impl DrawDistance {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Medium => "150 m",
            Self::Short => "75 m",
        }
    }
    pub(crate) fn meters(self) -> f32 {
        match self {
            Self::Full => 0.,
            Self::Medium => 150.,
            Self::Short => 75.,
        }
    }
}

/// Marks directional lights whose shadow casting is controlled by the graphics
/// menu. Spawn sites add this so the setting also covers maps loaded later.
#[derive(Component)]
pub(crate) struct ShadowCasterLight;

/// Coarse shadow budget. `Reduced` lowers the shadow-map resolution; `Off`
/// disables shadow casting entirely. Both are lossy and purely cosmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ShadowQuality {
    Default,
    Reduced,
    Off,
}
impl Default for ShadowQuality {
    fn default() -> Self {
        Self::Default
    }
}
impl ShadowQuality {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Reduced => "Reduced",
            Self::Off => "Off",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct GraphicsSettings {
    width: u32,
    height: u32,
    scale: u32,
    samples: u32,
    fps: u32,
    occlusion: bool,
    shadows: ShadowQuality,
    texture_detail: u32,
    env_props: bool,
    backdrops: bool,
    draw_distance: DrawDistance,
    hour: f32,
    day_speed: u32,
    ambient_level: Option<u32>,
}
impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            scale: 100,
            samples: 4,
            fps: 0,
            occlusion: true,
            shadows: ShadowQuality::Default,
            texture_detail: 100,
            env_props: true,
            backdrops: true,
            draw_distance: DrawDistance::Full,
            hour: 12.,
            day_speed: 60,
            ambient_level: None,
        }
    }
}
impl GraphicsSettings {
    fn validated(mut self) -> Self {
        self.ambient_level = self.ambient_level.map(|level| level.min(100));
        self.hour = if self.hour.is_finite() { self.hour.rem_euclid(24.) } else { 12. };
        if !DAY_SPEEDS.contains(&self.day_speed) { self.day_speed = 60; }
        if !RESOLUTIONS.contains(&(self.width, self.height)) {
            (self.width, self.height) = (1280, 800);
        }
        if !SCALES.contains(&self.scale) {
            self.scale = 100;
        }
        if ![1, 2, 4, 8].contains(&self.samples) {
            self.samples = 4;
        }
        if !LIMITS.contains(&self.fps) {
            self.fps = 0;
        }
        if !TEXTURE_DETAILS.contains(&self.texture_detail) {
            self.texture_detail = 100;
        }
        self
    }
    fn internal_size(&self, window: UVec2) -> UVec2 {
        (window * self.scale / 100).max(UVec2::ONE)
    }
    pub(crate) fn settings_path(asset_root: &std::path::Path) -> PathBuf {
        asset_root
            .parent()
            .unwrap_or(asset_root)
            .join("settings/graphics.json")
    }
    pub(crate) fn load_for(asset_root: &std::path::Path) -> Self {
        match std::fs::read(Self::settings_path(asset_root)) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes)
                .unwrap_or_default()
                .validated(),
            Err(_) => Self::default(),
        }
    }
}

/// Scene quality for render preparation, read from the persisted graphics
/// settings. Collision and simulation are unaffected.
pub(crate) fn scene_quality_for(asset_root: &std::path::Path) -> crate::map_render::SceneQuality {
    let settings = GraphicsSettings::load_for(asset_root);
    crate::map_render::SceneQuality {
        texture_scale: settings.texture_detail,
        env_props: settings.env_props,
        backdrops: settings.backdrops,
    }
}
#[derive(Resource)]
pub(crate) struct Menu {
    pub(crate) open: bool,
    selected: usize,
    settings: GraphicsSettings,
    path: PathBuf,
    supported_msaa: Vec<u32>,
    difficulty: Difficulty,
    status: String,
    maps: Vec<crate::map_library::Entry>,
    selected_map: usize,
    multiplayer: bool,
    browser: bool,
    daylight: bool,
}
impl Menu {
    pub(crate) fn ambient_brightness(&self, automatic: f32) -> f32 {
        self.settings.ambient_level.map_or(automatic, |level| level as f32 * 10.)
    }
    pub(crate) fn advance_day(&mut self, seconds: f32) -> f32 {
        if !self.open && self.settings.day_speed > 0 {
            self.settings.hour = (self.settings.hour + seconds * self.settings.day_speed as f32 / 3600.).rem_euclid(24.);
        }
        self.settings.hour
    }
    pub(crate) fn diagnostic_settings(&self) -> String {
        format!("{:?}", self.settings)
    }
    pub(crate) fn transition_finished(&mut self, status: String, resume: bool) {
        self.status = status;
        self.open = !resume;
    }
}
pub(crate) fn gameplay_active(menu: Option<Res<Menu>>) -> bool {
    menu.is_none_or(|m| !m.open)
}
#[derive(Resource)]
struct SceneTarget(Handle<Image>);
#[derive(Resource)]
struct FramePacer(Instant);
/// Current draw distance in metres; `0` disables culling.
#[derive(Resource, Default)]
pub(crate) struct DrawDistanceMeters(pub f32);
#[derive(Component)]
struct MenuRoot;
#[derive(Component)]
pub(crate) struct MenuRow(usize);
#[derive(Component)]
struct MenuLabel(usize);
#[derive(Component)]
struct StatusLabel;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MenuInput;

/// The presentation camera must exist before overlays select their UI target.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PresentationSetup;

pub(crate) struct GraphicsMenuPlugin;
impl Plugin for GraphicsMenuPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(FramePacer(Instant::now()))
            .init_resource::<DrawDistanceMeters>()
            .add_systems(PostStartup, setup.in_set(PresentationSetup))
            .add_systems(PreUpdate, interact.in_set(MenuInput).after(bevy::input::InputSystems))
            .add_systems(Update, (crate::map_render::advance_day, apply, labels).chain())
            .add_systems(Update, cull_distant_batches.after(apply))
            .add_systems(PostUpdate, crate::map_render::position_celestial_bodies.before(bevy::transform::TransformSystems::Propagate))
            .add_systems(Last, pace);
    }
}
fn setup(
    mut commands: Commands,
    config: Res<crate::config::Config>,
    mut images: ResMut<Assets<Image>>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    cameras: Query<Entity, With<Camera3d>>,
    adapter: Res<RenderAdapter>,
    mut time: ResMut<Time<Virtual>>,
) {
    let path = GraphicsSettings::settings_path(&config.asset_root);
    let settings = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<GraphicsSettings>(&bytes).unwrap_or_else(|e| {
            warn!("Graphics settings: {e}");
            GraphicsSettings::default()
        }),
        Err(_) => GraphicsSettings::default(),
    }
    .validated();
    let supported_msaa: Vec<_> = [1, 2, 4, 8]
        .into_iter()
        .filter(|&samples| {
            [
                TextureFormat::Rgba16Float,
                TextureFormat::Rgba8UnormSrgb,
                TextureFormat::Depth32Float,
            ]
            .into_iter()
            .all(|format| {
                adapter
                    .get_texture_format_features(format)
                    .flags
                    .sample_count_supported(samples)
            })
        })
        .collect();
    let mut settings = settings;
    // Reproducible A/B override; normal launches use the saved menu setting.
    match std::env::var("SKATE_OCCLUSION").as_deref() {
        Ok("0") => settings.occlusion = false,
        Ok("1") => settings.occlusion = true,
        _ => {}
    }
    match std::env::var("SKATE_SHADOWS").as_deref() {
        Ok("0") | Ok("off") => settings.shadows = ShadowQuality::Off,
        Ok("1") | Ok("reduced") => settings.shadows = ShadowQuality::Reduced,
        Ok("2") | Ok("default") => settings.shadows = ShadowQuality::Default,
        _ => {}
    }
    match std::env::var("SKATE_TEXTURES").as_deref() {
        Ok("50") | Ok("half") => settings.texture_detail = 50,
        Ok("100") | Ok("full") => settings.texture_detail = 100,
        _ => {}
    }
    match std::env::var("SKATE_PROPS").as_deref() {
        Ok("0") | Ok("off") => settings.env_props = false,
        Ok("1") | Ok("on") => settings.env_props = true,
        _ => {}
    }
    match std::env::var("SKATE_BACKDROPS").as_deref() {
        Ok("0") | Ok("off") => settings.backdrops = false,
        Ok("1") | Ok("on") => settings.backdrops = true,
        _ => {}
    }
    match std::env::var("SKATE_DISTANCE").as_deref() {
        Ok("full") => settings.draw_distance = DrawDistance::Full,
        Ok("medium") => settings.draw_distance = DrawDistance::Medium,
        Ok("short") => settings.draw_distance = DrawDistance::Short,
        _ => {}
    }
    if !supported_msaa.contains(&settings.samples) {
        settings.samples = 1;
    }
    window
        .resolution
        .set_physical_resolution(settings.width, settings.height);
    window.present_mode = PresentMode::AutoNoVsync;
    let size = settings.internal_size(window.physical_size());
    let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
    image.sampler = ImageSampler::linear();
    let target = images.add(image);
    for camera in &cameras {
        commands.entity(camera).insert((
            RenderTarget::Image(target.clone().into()),
            msaa(settings.samples),
        ));
        // Render the world before the presentation camera consumes its image.
        commands.entity(camera).insert(Camera {
            order: -1,
            ..default()
        });
    }
    let output = commands
        .spawn((Camera2d, Msaa::Off, IsDefaultUiCamera))
        .id();
    commands.spawn((
        Node {
            width: percent(100),
            height: percent(100),
            position_type: PositionType::Absolute,
            ..default()
        },
        ImageNode::new(target.clone()),
        UiTargetCamera(output),
    ));
    commands.spawn((MenuRoot, UiTargetCamera(output), GlobalZIndex(10), Node {
        display: Display::None, width:percent(100), height:percent(100), align_items:AlignItems::Center,
        justify_content:JustifyContent::Center, position_type:PositionType::Absolute, ..default()
    }, BackgroundColor(Color::srgba(0.015,0.025,0.04,0.88)))).with_children(|root| {
        root.spawn((Node { width:px(560),max_width:percent(95),padding:UiRect::all(px(18)),flex_direction:FlexDirection::Column,row_gap:px(4),border_radius:BorderRadius::all(px(12)),..default() },
            BackgroundColor(Color::srgb(0.035,0.055,0.08)))).with_children(|panel| {
            panel.spawn((Text::new("GAME MENU"),TextFont {font_size:32.,..default()},TextColor(Color::WHITE)));
            panel.spawn((Text::new("GAMEPLAY & GRAPHICS"),TextFont {font_size:16.,..default()},TextColor(Color::srgb(0.4,0.85,0.85))));
            for i in 0..22 {
                panel.spawn((Button, MenuRow(i), Node {width:percent(100),min_height:px(26),padding:UiRect::all(px(3)),align_items:AlignItems::Center,border_radius:BorderRadius::all(px(5)),..default()},
                    BackgroundColor(Color::srgb(0.08,0.11,0.15)))).with_children(|row| {
                    row.spawn((MenuLabel(i),Text::new(""),TextFont {font_size:18.,..default()},TextColor(Color::WHITE)));
                });
            }
            panel.spawn((StatusLabel,Text::new(""),TextFont {font_size:15.,..default()},TextColor(Color::srgb(0.65,0.75,0.8))));
            panel.spawn((Text::new("Click to cycle | Up/Down select | Left/Right change\nEsc resume | Changes save automatically"),TextFont {font_size:14.,..default()},TextColor(Color::srgb(0.65,0.75,0.8))));
        });
    });
    commands.insert_resource(SceneTarget(target));
    let maps = crate::map_library::discover(&config.asset_root);
    let selected_map = maps.iter().position(|m| m.path.as_ref() == config.map_path.as_ref()).unwrap_or(0);
    if config.start_paused { time.pause(); }
    commands.insert_resource(Menu {
        open: config.start_paused,
        selected: 0,
        settings,
        path,
        supported_msaa,
        difficulty: config.difficulty,
        status: String::new(),
        maps,
        selected_map,
        multiplayer: false,
        browser: false,
        daylight: false,
    });
}
fn msaa(samples: u32) -> Msaa {
    match samples {
        2 => Msaa::Sample2,
        4 => Msaa::Sample4,
        8 => Msaa::Sample8,
        _ => Msaa::Off,
    }
}
fn cycle<T: PartialEq + Copy>(values: &[T], value: T, direction: i32) -> T {
    let index = values.iter().position(|x| *x == value).unwrap_or(0) as i32;
    values[(index + direction).rem_euclid(values.len() as i32) as usize]
}
pub(crate) fn interact(
    mut config: ResMut<crate::config::Config>,
    mut transition: ResMut<crate::map_transition::MapTransition>,
    mut customiser: ResMut<crate::customiser::Customiser>,
    mut custom_models: ResMut<crate::custom_models::CustomModels>,
    (mut mods, panel): (ResMut<crate::modding::ModMenu>, Res<crate::modding::EnabledPanel>),
    nav: Res<crate::customiser::Navigation>,
    mut physics: ResMut<crate::physics::GamePhysics>,
    keys: Res<ButtonInput<KeyCode>>,
    mut menu: ResMut<Menu>,
    mut time: ResMut<Time<Virtual>>,
    buttons: Query<(&Interaction, &MenuRow), Changed<Interaction>>,
    mut exit: MessageWriter<AppExit>,
    mut net: ResMut<crate::multiplayer::Multiplayer>,
    mut typing: MessageReader<bevy::input::keyboard::KeyboardInput>,
    mut updater: ResMut<crate::updater::Updater>,
    mut travel: ResMut<crate::teleport_menu::Travel>,
) {
    if transition.busy() {
        menu.open = true;
        time.pause();
        return;
    }
    if mods.open || travel.open || travel.closed_this_frame || customiser.open || custom_models.open { return; }
    if keys.just_pressed(KeyCode::Escape) || nav.pressed & 0x10 != 0 {
        menu.open = !menu.open;
    }
    let mut action = None;
    for event in typing.read() {
        if !menu.open
            || !menu.multiplayer
            || menu.browser
            || menu.selected != 3
            || !event.state.is_pressed()
        {
            continue;
        }
        if event.key_code == KeyCode::Backspace {
            net.join_code.pop();
        }
        if let Some(text) = &event.text {
            for ch in text.chars().filter(|c| c.is_ascii_hexdigit() || *c == '-') {
                if net.join_code.len() < 40 {
                    net.join_code.push(ch);
                }
            }
        }
    }
    if menu.open {
        let rows = if menu.daylight { 4 } else if menu.multiplayer { 11 } else { 22 };
        if !panel.focused {
        if keys.just_pressed(KeyCode::ArrowUp) || nav.pressed & 1 != 0 {
            menu.selected = (menu.selected + rows - 1) % rows;
        }
        if keys.just_pressed(KeyCode::ArrowDown) || nav.pressed & 2 != 0 {
            menu.selected = (menu.selected + 1) % rows;
        }
        if keys.just_pressed(KeyCode::ArrowLeft) || nav.pressed & 4 != 0 {
            action = Some((menu.selected, -1));
        }
        if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::Enter) || nav.pressed & (8 | 0x1000) != 0 {
            action = Some((menu.selected, 1));
        }
        }
        for (interaction, row) in &buttons {
            if panel.dragging() { continue; }
            if *interaction == Interaction::Pressed {
                menu.selected = row.0;
                action = Some((row.0, 1));
            }
        }
    }
    if let Some((row, direction)) = action {
        let day_action = menu.daylight;
        if menu.daylight {
            match row {
                0 => menu.settings.hour = ((menu.settings.hour * 4.).round() + direction as f32).rem_euclid(96.) / 4.,
                1 => menu.settings.day_speed = cycle(DAY_SPEEDS, menu.settings.day_speed, direction),
                2 => {
                    // Auto, 0%, 5%, ... 100%, then Auto again.
                    let index = menu.settings.ambient_level.map_or(0, |level| level as i32 / 5 + 1);
                    let next = (index + direction).rem_euclid(22);
                    menu.settings.ambient_level = if next == 0 { None } else { Some((next as u32 - 1) * 5) };
                }
                _ => { menu.daylight = false; menu.selected = 16; }
            }
        } else if menu.browser {
            match row {
                0 => net.browse(0),
                1..=5 => net.join_row(row - 1),
                6 => {
                    let page = net.browser_page.saturating_sub(1);
                    net.browse(page);
                }
                7 => {
                    let page = net.browser_page + 1;
                    if page * 5 < net.browser_total {
                        net.browse(page);
                    }
                }
                8 => menu.open = false,
                9 => {
                    exit.write(AppExit::Success);
                }
                10 => {
                    menu.browser = false;
                    menu.selected = 6;
                }
                _ => {}
            }
        } else if menu.multiplayer {
            match row {
                0 => net.local(true),
                1 => net.local(false),
                2 => net.steam(true),
                3 => {}
                4 => net.steam(false),
                5 => net.leave(),
                6 => {
                    net.browse(0);
                    menu.browser = true;
                    menu.selected = 0;
                }
                8 => menu.open = false,
                9 => {
                    exit.write(AppExit::Success);
                }
                10 => {
                    menu.multiplayer = false;
                    menu.selected = 11;
                }
                _ => {}
            }
        } else {
            match row {
                0 => {
                    let size = cycle(
                        RESOLUTIONS,
                        (menu.settings.width, menu.settings.height),
                        direction,
                    );
                    (menu.settings.width, menu.settings.height) = size;
                }
                1 => menu.settings.scale = cycle(SCALES, menu.settings.scale, direction),
                2 => {
                    menu.settings.samples =
                        cycle(&menu.supported_msaa, menu.settings.samples, direction)
                }
                3 => menu.settings.fps = cycle(LIMITS, menu.settings.fps, direction),
                4 => menu.settings.occlusion = !menu.settings.occlusion,
                17 => menu.settings.shadows = cycle(SHADOW_QUALITIES, menu.settings.shadows, direction),
                18 => { menu.settings.texture_detail = cycle(TEXTURE_DETAILS, menu.settings.texture_detail, direction); menu.status = "Texture detail applies after loading a map".into(); },
                19 => { menu.settings.env_props = !menu.settings.env_props; menu.status = "Environment props apply after loading a map".into(); },
                20 => { menu.settings.backdrops = !menu.settings.backdrops; menu.status = "Backdrops apply after loading a map".into(); },
                21 => menu.settings.draw_distance = cycle(DRAW_DISTANCES, menu.settings.draw_distance, direction),
                5 => {
                    menu.difficulty = cycle(&Difficulty::ALL, menu.difficulty, direction);
                    physics.set_difficulty(menu.difficulty);
                    config.difficulty = menu.difficulty;
                    menu.status = match menu.difficulty.save(&config.asset_root) {
                        Ok(()) => "Difficulty saved".into(),
                        Err(e) => format!("Applied, but could not save: {e}"),
                    };
                }
                6 => {
                    menu.selected_map = (menu.selected_map as i32 + direction)
                        .rem_euclid(menu.maps.len() as i32)
                        as usize;
                    menu.status = "Choose Load map to switch".into();
                }
                7 => {
                    if net.active() {
                        menu.status = "Leave multiplayer before switching maps".into();
                    } else {
                        net.leave();
                        transition.request(menu.maps[menu.selected_map].clone());
                        menu.status = "Loading map...".into();
                    }
                }
                8 => menu.open = false,
                9 => {
                    exit.write(AppExit::Success);
                }
                10 => { custom_models.request_stock(); customiser.begin(); },
                11 => {
                    menu.multiplayer = true;
                    menu.selected = 0;
                }
                12 => custom_models.begin(),
                13 => menu.status = updater.open(false),
                14 => travel.open = true,
                15 => mods.begin(),
                16 => { menu.daylight = true; menu.selected = 0; menu.status = "Custom maps: change time, cycle speed and ambient light. Retail lighting stays authored.".into(); },
                _ => {}
            }
        }
        if ((row < 5 || (17..=21).contains(&row)) && !menu.multiplayer && !menu.daylight && !day_action) || (day_action && row < 3) {
            let save = (|| -> Result<(), String> {
                std::fs::create_dir_all(menu.path.parent().unwrap()).map_err(|e| e.to_string())?;
                std::fs::write(
                    &menu.path,
                    serde_json::to_vec_pretty(&menu.settings).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())
            })();
            menu.status = match save {
                Ok(()) => "Saved".into(),
                Err(e) => format!("Could not save: {e}"),
            };
        }
    }
    if menu.open && !net.active() {
        time.pause();
    } else {
        time.unpause();
    }
}
fn apply(
    mut commands: Commands,
    menu: Res<Menu>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    target: Res<SceneTarget>,
    mut images: ResMut<Assets<Image>>,
    mut cameras: Query<(Entity, &mut Msaa), With<Camera3d>>,
    mut lights: Query<&mut DirectionalLight, With<ShadowCasterLight>>,
    mut dir_shadow: Option<ResMut<bevy::light::DirectionalLightShadowMap>>,
    mut point_shadow: Option<ResMut<bevy::light::PointLightShadowMap>>,
    mut draw_distance: Option<ResMut<DrawDistanceMeters>>,
    mut previous: Local<Option<GraphicsSettings>>,
) {
    if previous
        .as_ref()
        .is_none_or(|p| p.width != menu.settings.width || p.height != menu.settings.height)
    {
        window
            .resolution
            .set_physical_resolution(menu.settings.width, menu.settings.height);
    }
    if previous
        .as_ref()
        .is_none_or(|p| p.samples != menu.settings.samples)
    {
        for (_, mut samples) in &mut cameras {
            *samples = msaa(menu.settings.samples);
        }
    }
    if previous
        .as_ref()
        .is_none_or(|p| p.occlusion != menu.settings.occlusion)
    {
        for (entity, _) in &cameras {
            if menu.settings.occlusion {
                commands
                    .entity(entity)
                    .insert((DepthPrepass, OcclusionCulling));
            } else {
                commands
                    .entity(entity)
                    .remove::<(DepthPrepass, OcclusionCulling)>();
            }
        }
        info!("GPU occlusion culling: {}", menu.settings.occlusion);
    }
    let size = menu.settings.internal_size(window.physical_size());
    if let Some(image) = images.get(&target.0) {
        if image.size() != size {
            images.get_mut(&target.0).unwrap().resize(Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            });
        }
    }
    // Shadow budget is applied every frame so lights spawned by a map load or
    // transition pick it up without needing a settings change.
    let (shadows_enabled, dir_size, point_size) = match menu.settings.shadows {
        ShadowQuality::Default => (true, 2048, 1024),
        ShadowQuality::Reduced => (true, 1024, 512),
        ShadowQuality::Off => (false, 2048, 1024),
    };
    if let Some(dir_shadow) = dir_shadow.as_mut() {
        if dir_shadow.size != dir_size {
            dir_shadow.size = dir_size;
        }
    }
    if let Some(point_shadow) = point_shadow.as_mut() {
        if point_shadow.size != point_size {
            point_shadow.size = point_size;
        }
    }
    for mut light in &mut lights {
        if light.shadows_enabled != shadows_enabled {
            light.shadows_enabled = shadows_enabled;
        }
    }
    if let Some(draw_distance) = draw_distance.as_mut() {
        draw_distance.0 = menu.settings.draw_distance.meters();
    }
    *previous = Some(menu.settings.clone());
}

/// Hides static batches whose nearest point lies entirely beyond the draw
/// distance. Nearest-point testing keeps every batch containing visible
/// geometry, so this never removes anything inside the range.
fn cull_distant_batches(
    camera: Query<&GlobalTransform, With<crate::camera::GameplayCamera>>,
    distance: Res<DrawDistanceMeters>,
    mut batches: Query<(&crate::skate_world::MapBatchBounds, &mut Visibility)>,
) {
    let limit = distance.0;
    let cam = camera.iter().next().map(GlobalTransform::translation);
    for (bounds, mut visibility) in &mut batches {
        let target = match (limit > 0., cam) {
            (true, Some(cam)) => {
                let nearest = (cam - bounds.center).abs() - bounds.half;
                let dist = nearest.max(Vec3::ZERO).length();
                // Small hysteresis so a batch on the boundary does not flicker.
                let was_visible = *visibility != Visibility::Hidden;
                let threshold = if was_visible { limit } else { limit * 0.9 };
                if dist <= threshold { Visibility::Inherited } else { Visibility::Hidden }
            }
            _ => Visibility::Inherited,
        };
        if *visibility != target {
            *visibility = target;
        }
    }
}
fn labels(
    menu: Res<Menu>,
    transition: Res<crate::map_transition::MapTransition>,
    time: Res<Time<Real>>,
    customiser: Res<crate::customiser::Customiser>,
    custom_models: Res<crate::custom_models::CustomModels>,
    travel: Res<crate::teleport_menu::Travel>,
    mods: Res<crate::modding::ModMenu>,
    net: Res<crate::multiplayer::Multiplayer>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut root: Single<&mut Node, With<MenuRoot>>,
    mut labels: Query<(&MenuLabel, &mut Text), Without<StatusLabel>>,
    mut status: Single<&mut Text, With<StatusLabel>>,
    mut buttons: Query<(&MenuRow, &Interaction, &mut BackgroundColor, &mut Node), Without<MenuRoot>>,
) {
    root.display = if menu.open && !mods.open && !travel.open && !customiser.open && !custom_models.open {
        Display::Flex
    } else {
        Display::None
    };
    if !menu.open {
        return;
    }
    let s = &menu.settings;
    let size = s.internal_size(window.physical_size());
    for (label, mut text) in &mut labels {
        **text = if menu.daylight {
            match label.0 {
                0 => { let minutes = (s.hour * 60.).floor() as u32 % 1440; format!("Time of day          {:02}:{:02}", minutes / 60, minutes % 60) },
                1 => if s.day_speed == 0 { "Cycle speed          Frozen".into() } else { format!("Cycle speed          {}x ({} min/day)", s.day_speed, 1440 / s.day_speed) },
                2 => match s.ambient_level {
                    Some(level) => format!("Ambient light        {level}%"),
                    None => "Ambient light        Auto (day/night)".into(),
                },
                3 => "Back".into(),
                _ => String::new(),
            }
        } else if menu.browser {
            match label.0 {
                0 => "Refresh public Steam lobbies".into(),
                1..=5 => net
                    .browser_rows
                    .get(label.0 - 1)
                    .map(|r| {
                        format!(
                            "{} | {}/{} | #{}{}",
                            r.map,
                            r.players,
                            r.capacity,
                            r.id % 100000,
                            if r.compatible {
                                ""
                            } else {
                                " | incompatible physics/protocol"
                            }
                        )
                    })
                    .unwrap_or_else(|| "--".into()),
                6 => "Previous page".into(),
                7 => "Next page".into(),
                8 => "Resume".into(),
                9 => "Quit game".into(),
                _ => "Back to multiplayer".into(),
            }
        } else if menu.multiplayer {
            match label.0 {
                0 => "Host local test (no Steam)".into(),
                1 => "Join local test (no Steam)".into(),
                2 => "Host via Steam / Spacewar".into(),
                3 => format!(
                    "Join code: {}{}",
                    net.join_code,
                    if menu.selected == 3 { "_" } else { "" }
                ),
                4 => "Join via Steam / Spacewar".into(),
                5 => "Leave multiplayer".into(),
                6 => "Browse public Steam lobbies".into(),
                7 => "Solo / local play does not require Steam".into(),
                8 => "Resume".into(),
                9 => "Quit game".into(),
                _ => "Back to gameplay & graphics".into(),
            }
        } else {
            match label.0 {
                0 => format!("Resolution          {} x {}", s.width, s.height),
                1 => format!(
                    "Internal resolution   {}%  ({} x {})",
                    s.scale, size.x, size.y
                ),
                2 => format!(
                    "MSAA                {}",
                    if s.samples == 1 {
                        "Off".into()
                    } else {
                        format!("{}x", s.samples)
                    }
                ),
                3 => format!(
                    "FPS limit             {}",
                    if s.fps == 0 {
                        "Unlimited".into()
                    } else {
                        s.fps.to_string()
                    }
                ),
                4 => format!(
                    "Occlusion culling     {}",
                    if s.occlusion { "On" } else { "Off" }
                ),
                5 => format!("Difficulty            {}", menu.difficulty.label()),
                6 => format!(
                    "Map                   {}",
                    menu.maps[menu.selected_map].label
                ),
                7 => if transition.busy() { "Loading map...".into() } else { "Load map".into() },
                8 => "Resume".into(),
                9 => "Quit game".into(),
                10 => "Character customiser".into(),
                12 => "Custom models".into(),
                13 => "Updates".into(),
                15 => "Mods".into(),
                14 => "Teleport…".into(),
                16 => "Day & night…".into(),
                17 => format!("Shadows               {}", s.shadows.label()),
                18 => format!("Texture detail        {}%", s.texture_detail),
                19 => format!("Environment props     {}", if s.env_props { "On" } else { "Off" }),
                20 => format!("Backdrops             {}", if s.backdrops { "On" } else { "Off" }),
                21 => format!("Draw distance         {}", s.draw_distance.label()),
                _ => "Multiplayer".into(),
            }
        };
    }
    ***status = if transition.busy() {
        format!("{} {}\nGameplay is paused. Please wait.", ["|", "/", "-", "\\"][(time.elapsed_secs() * 4.) as usize % 4], transition.label())
    } else if menu.browser {
        net.browser_status.clone()
    } else if menu.multiplayer {
        format!(
            "{}{}",
            net.status,
            if net.host_code.is_empty() {
                String::new()
            } else {
                format!("\nYour connection code: {}", net.host_code)
            }
        )
    } else {
        menu.status.clone()
    };
    for (row, interaction, mut color, mut node) in &mut buttons {
        node.display = if (menu.daylight && row.0 >= 4) || (menu.multiplayer && row.0 >= 11) { Display::None } else { Display::Flex };
        color.0 = if row.0 == menu.selected || *interaction == Interaction::Hovered {
            Color::srgb(0.10, 0.30, 0.34)
        } else {
            Color::srgb(0.08, 0.11, 0.15)
        };
    }
}
fn pace(menu: Option<Res<Menu>>, mut pacer: ResMut<FramePacer>) {
    let Some(menu) = menu else {
        return;
    };
    if menu.settings.fps > 0 {
        let period = Duration::from_secs_f64(1. / f64::from(menu.settings.fps));
        if let Some(wait) = period.checked_sub(pacer.0.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    pacer.0 = Instant::now();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn culling_can_toggle_with_msaa_and_render_scale_changes() {
        let mut app = App::new();
        let mut images = Assets::<Image>::default();
        let target = images.add(Image::new_target_texture(
            1280,
            800,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));
        app.insert_resource(SceneTarget(target.clone()))
            .insert_resource(images)
            .insert_resource(Menu {
                open: false, selected: 0, settings: GraphicsSettings::default(),
                difficulty: Difficulty::Easy, path: PathBuf::new(), supported_msaa: vec![1, 2, 4, 8], status: String::new(),
                multiplayer: false, browser: false, daylight: false,
                maps: vec![crate::map_library::Entry { label: "Test world".into(), path: None }], selected_map: 0,
            })
            .add_systems(Update, apply);
        {
            let mut menu = app.world_mut().resource_mut::<Menu>();
            menu.settings.hour = 23.5;
            menu.settings.day_speed = 60;
            assert!((menu.advance_day(60.) - 0.5).abs() < 0.0001);
            menu.open = true;
            assert_eq!(menu.advance_day(60.), 0.5);
            menu.open = false;
            menu.settings.day_speed = 0;
            assert_eq!(menu.advance_day(60.), 0.5);
        }
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        let camera = app.world_mut().spawn((Camera3d::default(), Msaa::Off)).id();
        app.update();
        assert!(app.world().entity(camera).contains::<OcclusionCulling>());
        assert!(app.world().entity(camera).contains::<DepthPrepass>());
        {
            let mut menu = app.world_mut().resource_mut::<Menu>();
            menu.settings.occlusion = false;
            menu.settings.samples = 1;
            menu.settings.scale = 67;
        }
        app.update();
        assert!(!app.world().entity(camera).contains::<OcclusionCulling>());
        assert!(!app.world().entity(camera).contains::<DepthPrepass>());
        assert_eq!(*app.world().get::<Msaa>(camera).unwrap(), Msaa::Off);
        assert_eq!(
            app.world()
                .resource::<Assets<Image>>()
                .get(&target)
                .unwrap()
                .size(),
            UVec2::new(857, 536)
        );
        {
            let mut menu = app.world_mut().resource_mut::<Menu>();
            menu.settings.occlusion = true;
            menu.settings.samples = 8;
        }
        app.update();
        assert!(app.world().entity(camera).contains::<OcclusionCulling>());
        assert!(app.world().entity(camera).contains::<DepthPrepass>());
        assert_eq!(*app.world().get::<Msaa>(camera).unwrap(), Msaa::Sample8);
    }
    #[test]
    fn invalid_saved_values_fall_back() {
        let settings: GraphicsSettings =
            serde_json::from_str(r#"{"width":0,"height":999999,"scale":0,"samples":3,"fps":1}"#)
                .unwrap();
        assert_eq!(settings.validated(), GraphicsSettings::default());
    }
    #[test]
    fn scaled_target_and_cycle_boundaries() {
        let s = GraphicsSettings {
            scale: 50,
            ..default()
        };
        assert_eq!(
            s.internal_size(UVec2::new(1920, 1080)),
            UVec2::new(960, 540)
        );
        assert_eq!(s.internal_size(UVec2::ZERO), UVec2::ONE);
        assert_eq!(cycle(LIMITS, 0, -1), 240);
        assert_eq!(cycle(LIMITS, 240, 1), 0);
    }
}
