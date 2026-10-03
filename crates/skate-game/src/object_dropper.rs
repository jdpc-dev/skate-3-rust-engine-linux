//! Host-side Object Dropper MVP.
//!
//! Places authored DMO props exported by `tools/asset_pipeline/dynamic_props.py`
//! into the live static world. This is a deliberate host reimplementation: the
//! catalog, contact triangles and placement rules are not recovered native
//! Object Dropper behaviour. Placed objects are static; they are not movable
//! DMO simulation bodies. See `docs/object-dropper.md`.
use bevy::prelude::*;
use serde::Deserialize;
use skate_core::{
    math::Vector3,
    physics::{
        board_world::WorldTriangle, collision::TriangleFeature, contact::RetailContactMaterial,
    },
};
use skate_data::skate_map::SkateMap;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    config::Config,
    input::ControllerInput,
    map_transition::{CurrentMap, MapTransition},
    physics::{GamePhysics, SkaterRuntime},
    replay::Replay,
    skate_world::ObjectMaterial,
};

const PLACED_CAP: usize = 32;
const PLACE_DISTANCE: f32 = 4.0;
const ROTATION_STEP: f32 = std::f32::consts::FRAC_PI_4 / 3.0;

#[derive(Deserialize)]
struct Catalog {
    version: u32,
    #[allow(dead_code)]
    map: String,
    templates: Vec<CatalogEntry>,
}
#[derive(Deserialize)]
struct CatalogEntry {
    id: String,
    name: String,
    category: String,
    file: String,
    bounds: [[f32; 3]; 2],
}

pub(crate) struct PropTemplate {
    pub id: String,
    pub name: String,
    pub category: String,
    file: PathBuf,
    #[allow(dead_code)]
    pub bounds: [[f32; 3]; 2],
}

pub(crate) struct PropAsset {
    meshes: Vec<(Handle<Mesh>, ObjectMaterial)>,
    /// Local-space contact triangles with the packed authored surface code.
    collision: Vec<([Vector3; 3], u16)>,
    /// Local-space coping/ledge edges for the off-board ground query (drop-in).
    drop_edges: Vec<(Vector3, Vector3)>,
}

/// Retail material tuning used to build object materials exactly like the map.
#[derive(Resource, Default)]
pub(crate) struct DropperTuning(pub(crate) crate::retail_render::MaterialTuning);

/// Asset banks shared by preview and placement. Bundled to keep the dropper
/// system within Bevy's system-parameter arity limit.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct DropperAssets<'w> {
    pub(crate) meshes: ResMut<'w, Assets<Mesh>>,
    pub(crate) materials: ResMut<'w, Assets<StandardMaterial>>,
    pub(crate) retail: ResMut<'w, Assets<crate::retail_render::RetailWorldMaterial>>,
    pub(crate) images: ResMut<'w, Assets<Image>>,
}

#[derive(Resource)]
pub(crate) struct PropTemplates {
    generation: u64,
    #[allow(dead_code)]
    map: String,
    pub templates: Vec<PropTemplate>,
    loaded: HashMap<String, Arc<PropAsset>>,
}
impl Default for PropTemplates {
    fn default() -> Self {
        Self {
            generation: u64::MAX,
            map: String::new(),
            templates: Vec::new(),
            loaded: HashMap::new(),
        }
    }
}

struct Placed {
    entity: Entity,
    range: std::ops::Range<usize>,
    /// Group id of this object's registered off-board coping edges, if any.
    host_group: Option<u64>,
}

#[derive(Resource, Default)]
pub(crate) struct Dropper {
    pub open: bool,
    pub available: bool,
    pub selected: usize,
    pub yaw: f32,
    revision: u64,
    last_batch: u64,
    next_host_group: u64,
    preview: Option<Entity>,
    preview_id: Option<String>,
    placed: Vec<Placed>,
}

#[derive(Component)]
struct DropperRoot;

pub(crate) struct ObjectDropperPlugin;
impl Plugin for ObjectDropperPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PropTemplates>()
            .init_resource::<Dropper>()
            .init_resource::<DropperTuning>()
            .add_systems(PreUpdate, refresh)
            // Use the same per-frame menu navigation the travel menu uses: it
            // edge-detects the raw pad in PreUpdate, so it does not race the
            // FixedUpdate pad publication.
            .add_systems(
                Update,
                run.run_if(crate::graphics_menu::gameplay_active)
                    .before(draw_ui),
            )
            .add_systems(Update, (draw_ui, toggle_hud, close_when_paused).chain());
    }
}

fn close_when_paused(mut dropper: ResMut<Dropper>, menu: Res<crate::graphics_menu::Menu>) {
    if dropper.open && !crate::graphics_menu::gameplay_active(Some(menu)) {
        dropper.open = false;
        dropper.revision = dropper.revision.wrapping_add(1);
    }
}

fn load_catalog(folder: &Path) -> Result<Vec<PropTemplate>, String> {
    let bytes = std::fs::read(folder.join("catalog.json")).map_err(|e| e.to_string())?;
    let catalog: Catalog = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if catalog.version != 1 {
        return Err("Unsupported object-dropper catalog version".into());
    }
    let mut templates = Vec::with_capacity(catalog.templates.len());
    for entry in catalog.templates {
        let path = Path::new(&entry.file);
        if entry.id.is_empty()
            || entry.name.is_empty()
            || entry.category.is_empty()
            || path.components().count() != 1
            || !entry.bounds.iter().flatten().all(|v| v.is_finite())
        {
            return Err("Invalid object-dropper catalog entry".into());
        }
        templates.push(PropTemplate {
            id: entry.id,
            name: entry.name,
            category: entry.category,
            file: folder.join(path),
            bounds: entry.bounds,
        });
    }
    Ok(templates)
}

fn refresh(
    mut commands: Commands,
    map: Res<CurrentMap>,
    config: Res<Config>,
    mut templates: ResMut<PropTemplates>,
    mut dropper: ResMut<Dropper>,
    mut tuning: ResMut<DropperTuning>,
) {
    if templates.generation == map.generation {
        return;
    }
    tuning.0 = crate::retail_render::MaterialTuning::load(&config.asset_root);
    templates.generation = map.generation;
    templates.map = map.name.clone();
    templates.templates.clear();
    templates.loaded.clear();
    if let Some(entity) = dropper.preview.take() {
        commands.entity(entity).despawn();
    }
    dropper.preview_id = None;
    for placed in dropper.placed.drain(..) {
        commands.entity(placed.entity).despawn();
    }
    dropper.open = false;
    dropper.available = false;
    dropper.selected = 0;
    dropper.yaw = 0.;
    dropper.last_batch = u64::MAX;
    dropper.revision = dropper.revision.wrapping_add(1);
    if map.path.is_none() {
        return;
    }
    let folder = config
        .asset_root
        .join("private/dropper-templates")
        .join(&map.name);
    match load_catalog(&folder) {
        Ok(list) if !list.is_empty() => {
            templates.templates = list;
            dropper.available = true;
            info!(
                "Object dropper catalog loaded: {} templates from {}",
                templates.templates.len(),
                folder.display()
            );
        }
        Ok(_) => {}
        Err(e) => debug!("Object dropper catalog unavailable: {e}"),
    }
}

fn face_normal(points: &[[f32; 3]], face: &[u32]) -> [f32; 3] {
    let a = points[face[0] as usize];
    let b = points[face[1] as usize];
    let c = points[face[2] as usize];
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 1e-9 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        [0., 1., 0.]
    }
}

/// Coping/ledge edges for drop-in: top-band edges that are open boundaries or
/// hard creases (transition-to-deck lip) and are more horizontal than vertical.
/// This is a host heuristic; retail maps use authored coping splines instead.
fn build_drop_edges(map: &SkateMap) -> Vec<(Vector3, Vector3)> {
    let points: Vec<[f32; 3]> = map.geometry.vertices.iter().map(|v| v.position).collect();
    if points.len() < 3 || map.geometry.indices.len() < 3 {
        return Vec::new();
    }
    let max_y = points.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    let min_y = points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    let band = ((max_y - min_y) * 0.25).clamp(0.2, 0.6);
    // Canonical edge -> (count, reference normal, endpoints, crease).
    let mut edges: HashMap<(u32, u32), (u32, [f32; 3], [f32; 3], [f32; 3], bool)> = HashMap::new();
    for face in map.geometry.indices.chunks_exact(3) {
        let normal = face_normal(&points, face);
        for corner in 0..3 {
            let (a, b) = (face[corner], face[(corner + 1) % 3]);
            let key = (a.min(b), a.max(b));
            let entry = edges
                .entry(key)
                .or_insert((0, normal, points[a as usize], points[b as usize], false));
            if entry.0 >= 1 {
                let dot = entry.1[0] * normal[0] + entry.1[1] * normal[1] + entry.1[2] * normal[2];
                if dot < 0.86 {
                    entry.4 = true;
                }
            }
            entry.0 += 1;
        }
    }
    let mut out = Vec::new();
    for (_, (count, _, start, end, crease)) in edges {
        if count != 1 && !crease {
            continue;
        }
        if start[1].min(end[1]) < max_y - band {
            continue;
        }
        let dy = (start[1] - end[1]).abs();
        let horizontal = ((start[0] - end[0]).powi(2) + (start[2] - end[2]).powi(2)).sqrt();
        if horizontal <= 1e-4 || dy > horizontal {
            continue;
        }
        out.push((
            Vector3::new(start[0], start[1], start[2]),
            Vector3::new(end[0], end[1], end[2]),
        ));
    }
    out
}

fn build_collision(map: &SkateMap) -> Vec<([Vector3; 3], u16)> {
    let mut out = Vec::with_capacity(map.geometry.indices.len() / 3);
    for face in map.geometry.indices.chunks_exact(3) {
        let points = [face[0], face[1], face[2]].map(|index| {
            let position = map.geometry.vertices[index as usize].position;
            Vector3::new(position[0], position[1], position[2])
        });
        let material = map.geometry.vertices[face[0] as usize].material;
        let packed = material
            .checked_sub(1)
            .and_then(|i| map.materials.get(i as usize))
            .map(|m| (m.audio | (m.physics << 7) | (m.pattern << 12)) as u16)
            .unwrap_or(0);
        out.push((points, packed));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn load_asset(
    templates: &mut PropTemplates,
    tuning: &crate::retail_render::MaterialTuning,
    id: &str,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    retail_materials: &mut Assets<crate::retail_render::RetailWorldMaterial>,
    images: &mut Assets<Image>,
) -> Result<Arc<PropAsset>, String> {
    if let Some(asset) = templates.loaded.get(id) {
        return Ok(asset.clone());
    }
    let template = templates
        .templates
        .iter()
        .find(|t| t.id == id)
        .ok_or("Unknown object template")?;
    let bytes = std::fs::read(&template.file).map_err(|e| e.to_string())?;
    let map = SkateMap::parse_render_only(&bytes).map_err(|e| e.to_string())?;
    if map.geometry.vertices.is_empty() || map.geometry.indices.len() < 3 {
        return Err("Object template has no geometry".into());
    }
    let asset = Arc::new(PropAsset {
        meshes: crate::skate_world::object_assets(
            &map,
            tuning,
            100,
            meshes,
            materials,
            retail_materials,
            images,
        ),
        collision: build_collision(&map),
        drop_edges: if template.category == "Ramp" {
            build_drop_edges(&map)
        } else {
            Vec::new()
        },
    });
    templates.loaded.insert(id.to_owned(), asset.clone());
    Ok(asset)
}

fn spawn_prop(commands: &mut Commands, asset: &PropAsset, transform: Transform) -> Entity {
    commands
        .spawn(transform)
        .with_children(|parent| {
            for (mesh, material) in &asset.meshes {
                match material {
                    ObjectMaterial::Retail(material) => {
                        parent.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
                    }
                    ObjectMaterial::Standard(material) => {
                        parent.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
                    }
                }
            }
        })
        .id()
}

fn build_world_triangles(
    asset: &PropAsset,
    transform: Mat4,
    floor: RetailContactMaterial,
) -> (Vec<WorldTriangle>, Vec<u16>) {
    let mut triangles = Vec::with_capacity(asset.collision.len());
    let mut surfaces = Vec::with_capacity(asset.collision.len());
    for (points, packed) in &asset.collision {
        let transformed = points.map(|point| {
            let world = transform.transform_point3(Vec3::new(point.x, point.y, point.z));
            Vector3::new(world.x, world.y, world.z)
        });
        if let Some(triangle) = WorldTriangle::from_vertices(
            transformed,
            floor,
            0,
            TriangleFeature::ONE_SIDED | TriangleFeature::USE_EDGE_COSINES | 0xe0,
            [1.; 3],
            0.,
        ) {
            triangles.push(triangle);
            surfaces.push(*packed);
        }
    }
    (triangles, surfaces)
}

fn ground_height(physics: &GamePhysics, x: f32, fallback_y: f32, z: f32) -> f32 {
    let start = Vector3::new(x, fallback_y + 8., z);
    let end = Vector3::new(x, fallback_y - 60., z);
    match physics.world().query_thin_line(start, end) {
        Ok(Some(hit)) => hit.geometry.position.y,
        _ => fallback_y,
    }
}

fn remove_placed(
    commands: &mut Commands,
    physics: &mut GamePhysics,
    dropper: &mut Dropper,
    index: usize,
) {
    let placed = dropper.placed.remove(index);
    if let Err(e) = physics.remove_static_geometry(placed.range.clone()) {
        warn!("Object dropper delete failed: {e}");
    }
    if let Some(group) = placed.host_group {
        physics.remove_host_edges(group);
    }
    commands.entity(placed.entity).despawn();
    let removed = placed.range.len();
    for remaining in &mut dropper.placed {
        if remaining.range.start >= placed.range.start {
            remaining.range =
                (remaining.range.start - removed)..(remaining.range.end - removed);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut commands: Commands,
    mut input: ResMut<ControllerInput>,
    nav: Res<crate::customiser::Navigation>,
    keys: Res<ButtonInput<KeyCode>>,
    mut dropper: ResMut<Dropper>,
    mut templates: ResMut<PropTemplates>,
    mut physics: ResMut<GamePhysics>,
    mut skater: ResMut<SkaterRuntime>,
    replay: Res<Replay>,
    transition: Res<MapTransition>,
    menu: Res<crate::graphics_menu::Menu>,
    mut assets: DropperAssets,
    tuning: Res<DropperTuning>,
    mut preview_query: Query<
        &mut Transform,
        (Without<DropperRoot>, Without<crate::camera::GameplayCamera>),
    >,
    camera: Query<&Transform, With<crate::camera::GameplayCamera>>,
    mut toggle_prev: Local<bool>,
) {
    let toggle_held = keys.pressed(KeyCode::F7);
    let toggle = toggle_held && !*toggle_prev;
    *toggle_prev = toggle_held;
    let actions = input.object_dropper_actions();
    let fresh = dropper.last_batch != input.consumed_batches;
    if fresh {
        dropper.last_batch = input.consumed_batches;
    }
    let suspended = replay.active
        || transition.busy()
        || !crate::graphics_menu::gameplay_active(Some(menu));
    if dropper.open && suspended {
        dropper.open = false;
        dropper.revision = dropper.revision.wrapping_add(1);
    }
    // Menu navigation is D-pad only (`pressed_buttons` never includes the
    // stick), so the player can keep steering with the joystick. The Pad path
    // is a fallback for the LB+B chord.
    let lb = nav.held & 0x0100 != 0;
    let open = (lb && nav.pressed_buttons & 0x2000 != 0)
        || (actions.open && fresh)
        || toggle;
    let confirm = nav.pressed_buttons & 0x1000 != 0
        || actions.confirm && fresh
        || keys.just_pressed(KeyCode::Enter);
    let cancel = nav.pressed_buttons & 0x2000 != 0 || (actions.cancel && fresh);
    let delete = nav.pressed_buttons & 0x4000 != 0
        || (actions.delete && fresh)
        || keys.just_pressed(KeyCode::Backspace);
    let up = nav.pressed_buttons & 1 != 0 || (actions.up && fresh);
    let down = nav.pressed_buttons & 2 != 0 || (actions.down && fresh);
    let left = nav.pressed_buttons & 4 != 0
        || (actions.left && fresh)
        || keys.just_pressed(KeyCode::BracketLeft);
    let right = nav.pressed_buttons & 8 != 0
        || (actions.right && fresh)
        || keys.just_pressed(KeyCode::BracketRight);
    // LB+B sets both `open` and `cancel` on the same press; never let the same
    // press close the menu it just opened.
    let mut opened_this_tick = false;
    if open && !dropper.open {
        if !dropper.available {
            debug!("Object dropper requested but this map has no catalog");
        } else if suspended {
            debug!("Object dropper unavailable during pause/replay/map change");
        } else {
            info!("Object dropper opened ({} templates)", templates.templates.len());
            dropper.open = true;
            dropper.revision = dropper.revision.wrapping_add(1);
            opened_this_tick = true;
            // Bajar al jugador de la tabla: reuse the manual teleport reset with
            // on_board=false (the same path as vehicle ejection), which enters
            // the off-board biped ground state.
            if skater.player_input.physical.state.category_12 != 500 {
                let transform = skater.animated_skeleton.roots.animation_to_world;
                if skater.player_input.request_teleport(transform).is_ok() {
                    skater.teleport_state.request_manual(transform, false);
                }
            }
        }
    } else if open && dropper.open {
        info!("Object dropper closed");
        dropper.open = false;
        dropper.revision = dropper.revision.wrapping_add(1);
    }
    // Block jump/tricks/remount while the menu is open; movement stays live.
    input.suppress_gameplay = dropper.open;
    if dropper.open {
        let count = templates.templates.len();
        if count != 0 {
            if down {
                dropper.selected = (dropper.selected + 1) % count;
                dropper.revision = dropper.revision.wrapping_add(1);
            } else if up {
                dropper.selected = (dropper.selected + count - 1) % count;
                dropper.revision = dropper.revision.wrapping_add(1);
            }
            if left {
                dropper.yaw += ROTATION_STEP;
            } else if right {
                dropper.yaw -= ROTATION_STEP;
            }
            if delete && !dropper.placed.is_empty() {
                let last = dropper.placed.len() - 1;
                remove_placed(&mut commands, &mut physics, &mut dropper, last);
            }
            if cancel && !opened_this_tick {
                dropper.open = false;
                dropper.revision = dropper.revision.wrapping_add(1);
            }
        }
    }
    if !dropper.open {
        if dropper.preview.is_some() {
            if let Some(entity) = dropper.preview.take() {
                commands.entity(entity).despawn();
            }
            dropper.preview_id = None;
        }
        return;
    }
    let Some(template) = templates.templates.get(dropper.selected) else {
        return;
    };
    let id = template.id.clone();
    let root = skater.animated_skeleton.roots.animation_to_world;
    let position = Vec3::from_slice(&root[3][..3]);
    // Spawn in front of the player. The gameplay camera looks from behind the
    // skater, so its horizontal forward points past them into the world. Fall
    // back to the inverted animation root because the raw root row is the
    // skater's back axis.
    let forward = camera
        .iter()
        .next()
        .and_then(|camera| {
            let forward = *camera.forward();
            Vec3::new(forward.x, 0., forward.z).try_normalize()
        })
        .unwrap_or_else(|| {
            let mut fallback = Vec3::new(root[2][0], 0., root[2][2]);
            fallback = fallback.try_normalize().unwrap_or(Vec3::Z);
            -fallback
        });
    let target = position + forward * PLACE_DISTANCE;
    let ground = ground_height(&physics, target.x, position.y, target.z);
    let transform = Transform {
        translation: Vec3::new(target.x, ground, target.z),
        rotation: Quat::from_rotation_y(dropper.yaw),
        ..default()
    };
    if dropper.preview_id.as_deref() != Some(id.as_str()) || dropper.preview.is_none() {
        if let Some(entity) = dropper.preview.take() {
            commands.entity(entity).despawn();
        }
        match load_asset(&mut templates, &tuning.0, &id, &mut assets.meshes, &mut assets.materials, &mut assets.retail, &mut assets.images) {
            Ok(asset) => {
                dropper.preview = Some(spawn_prop(&mut commands, &asset, transform));
                dropper.preview_id = Some(id.clone());
            }
            Err(e) => {
                warn!("Object dropper template {id}: {e}");
                dropper.preview_id = None;
            }
        }
    } else if let Some(entity) = dropper.preview {
        if let Ok(mut current) = preview_query.get_mut(entity) {
            *current = transform;
        }
    }
    if !confirm {
        return;
    }
    let asset = match templates.loaded.get(&id).cloned() {
        Some(asset) => asset,
        None => match load_asset(&mut templates, &tuning.0, &id, &mut assets.meshes, &mut assets.materials, &mut assets.retail, &mut assets.images) {
            Ok(asset) => asset,
            Err(e) => {
                warn!("Object dropper template {id}: {e}");
                return;
            }
        },
    };
    let entity = spawn_prop(&mut commands, &asset, transform);
    let floor = physics.floor_material();
    let matrix = Mat4::from_rotation_translation(transform.rotation, transform.translation);
    let (triangles, surfaces) = build_world_triangles(&asset, matrix, floor);
    let mut host_group = None;
    if !asset.drop_edges.is_empty() {
        let group = dropper.next_host_group;
        dropper.next_host_group = dropper.next_host_group.wrapping_add(1);
        let edges = asset.drop_edges.iter().map(|(start, end)| {
            let a = matrix.transform_point3(Vec3::new(start.x, start.y, start.z));
            let b = matrix.transform_point3(Vec3::new(end.x, end.y, end.z));
            ([a.x, a.y, a.z], [b.x, b.y, b.z])
        });
        physics.add_host_edges(group, edges);
        host_group = Some(group);
    }
    match physics.append_static_geometry(triangles, surfaces) {
        Ok(range) => {
            dropper.placed.push(Placed {
                entity,
                range,
                host_group,
            });
            while dropper.placed.len() > PLACED_CAP {
                remove_placed(&mut commands, &mut physics, &mut dropper, 0);
            }
        }
        Err(e) => {
            warn!("Object dropper placement rejected: {e}");
            if let Some(group) = host_group {
                physics.remove_host_edges(group);
            }
            commands.entity(entity).despawn();
        }
    }
}

fn draw_ui(
    mut commands: Commands,
    dropper: Res<Dropper>,
    templates: Res<PropTemplates>,
    roots: Query<Entity, With<DropperRoot>>,
    mut revision: Local<u64>,
) {
    if !dropper.open {
        if *revision != u64::MAX {
            for entity in &roots {
                commands.entity(entity).despawn();
            }
            *revision = u64::MAX;
        }
        return;
    }
    if *revision == dropper.revision {
        return;
    }
    *revision = dropper.revision;
    for entity in &roots {
        commands.entity(entity).despawn();
    }
    let selected = dropper.selected;
    commands
        .spawn((
            DropperRoot,
            GlobalZIndex(12),
            Node {
                position_type: PositionType::Absolute,
                left: px(16.),
                top: px(150.),
                width: px(360.),
                max_height: percent(70.),
                padding: UiRect::all(px(12.)),
                flex_direction: FlexDirection::Column,
                row_gap: px(4.),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.025, 0.04, 0.92)),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("OBJECT DROPPER"),
                TextFont { font_size: 22., ..default() },
            ));
            root.spawn((
                Text::new("A place / B close / X delete last / D-pad move / [ ] rotate"),
                TextFont { font_size: 12., ..default() },
                TextColor(Color::srgb(0.7, 0.78, 0.82)),
            ));
            root.spawn(Node {
                max_height: px(420.),
                overflow: Overflow::scroll_y(),
                flex_direction: FlexDirection::Column,
                ..default()
            })
            .with_children(|list| {
                if templates.templates.is_empty() {
                    list.spawn(Text::new("No object catalog for this map."));
                }
                let total = templates.templates.len();
                let window = 14usize;
                let start = selected
                    .saturating_sub(window / 2)
                    .min(total.saturating_sub(window));
                let end = (start + window).min(total);
                if start > 0 {
                    list.spawn((
                        Text::new(format!("^ {start} more")),
                        TextFont { font_size: 12., ..default() },
                        TextColor(Color::srgb(0.55, 0.6, 0.65)),
                    ));
                }
                let mut category = "";
                for index in start..end {
                    let template = &templates.templates[index];
                    if template.category != category {
                        category = &template.category;
                        list.spawn((
                            Text::new(category.to_uppercase()),
                            TextFont { font_size: 14., ..default() },
                            TextColor(Color::srgb(0.6, 0.85, 0.9)),
                        ));
                    }
                    list.spawn((
                        Text::new(template.name.clone()),
                        TextFont { font_size: 16., ..default() },
                        TextColor(if index == selected {
                            Color::srgb(1.0, 0.95, 0.6)
                        } else {
                            Color::WHITE
                        }),
                    ));
                }
                if end < total {
                    list.spawn((
                        Text::new(format!("v {} more", total - end)),
                        TextFont { font_size: 12., ..default() },
                        TextColor(Color::srgb(0.55, 0.6, 0.65)),
                    ));
                }
            });
        });
}

fn toggle_hud(
    dropper: Res<Dropper>,
    hud: Res<crate::session_marker::hud::DropperHud>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut applied: Local<Option<bool>>,
) {
    if *applied == Some(dropper.available) {
        return;
    }
    *applied = Some(dropper.available);
    for (handle, base) in &hud.0 {
        if let Some(material) = materials.get_mut(handle) {
            let alpha = if dropper.available {
                (base[3] / 0.3).min(1.0)
            } else {
                base[3]
            };
            material.color = Color::srgba(base[0], base[1], base[2], alpha);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skate_data::skate_map::{Geometry, Material, Vertex};

    fn triangle_map() -> SkateMap {
        let vertex = |x: f32, z: f32| Vertex {
            position: [x, 0., z],
            normal: [0., 1., 0.],
            uv: [0., 0.],
            lightmap_uv: [0., 0.],
            material: 1,
            decal_uv: None,
            tangent_frame: None,
        };
        SkateMap {
            version: 15,
            name: "Template".into(),
            spawn: [0.; 3],
            heading: 0.,
            environment: vec![],
            materials: vec![Material {
                name: "m".into(),
                flags: 0,
                friction: 0.7,
                restitution: 0.2,
                color: [1., 1., 1.],
                roughness: 0.5,
                emissive: 0.,
                textures: [0; 5],
                indirect_strength: 0.,
                alpha_mode: 0,
                alpha_cutoff: 0.5,
                audio: 3,
                physics: 2,
                pattern: 1,
                depth_layer: None,
                retail_definition: None,
            }],
            textures: vec![],
            geometry: Geometry {
                vertices: vec![vertex(0., 0.), vertex(1., 0.), vertex(0., 1.)],
                indices: vec![0, 1, 2],
                collision: vec![],
            },
            rails: vec![],
            doors: vec![],
            lights: vec![],
            routes: vec![],
            extensions: vec![],
        }
    }

    #[test]
    fn collision_uses_render_geometry_and_authored_surface_code() {
        let collision = build_collision(&triangle_map());
        assert_eq!(collision.len(), 1);
        let (points, packed) = collision[0];
        assert_eq!(points[0], Vector3::new(0., 0., 0.));
        assert_eq!(packed, 3 | (2 << 7) | (1 << 12));
    }

    #[test]
    fn drop_edges_keep_the_horizontal_top_rim() {
        // A vertical wall quad with a horizontal top edge at y = 1.
        let vertex = |x: f32, y: f32| Vertex {
            position: [x, y, 0.],
            normal: [0., 0., 1.],
            uv: [0., 0.],
            lightmap_uv: [0., 0.],
            material: 1,
            decal_uv: None,
            tangent_frame: None,
        };
        let mut map = triangle_map();
        map.geometry = Geometry {
            vertices: vec![vertex(0., 0.), vertex(1., 0.), vertex(0., 1.), vertex(1., 1.)],
            indices: vec![0, 1, 2, 1, 3, 2],
            collision: vec![],
        };
        let edges = build_drop_edges(&map);
        assert!(
            edges.iter().any(|(a, b)| {
                ((a.y - 1.).abs() < 1e-5 && (b.y - 1.).abs() < 1e-5)
            }),
            "top rim edge must be present: {edges:?}"
        );
        // Vertical side edges must be excluded.
        assert!(
            edges
                .iter()
                .all(|(a, b)| !((a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() > 1e-6)),
            "only horizontal top edges are kept: {edges:?}"
        );
    }

    #[test]
    fn placed_triangles_follow_the_placement_transform() {
        let asset = PropAsset {
            meshes: vec![],
            collision: vec![(
                [
                    Vector3::new(0., 0., 0.),
                    Vector3::new(1., 0., 0.),
                    Vector3::new(0., 0., 1.),
                ],
                17,
            )],
            drop_edges: vec![],
        };
        let floor = RetailContactMaterial {
            static_friction: 0.7,
            dynamic_friction: 0.4,
            restitution: 0.2,
        };
        let transform = Mat4::from_rotation_translation(
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            Vec3::new(10., 5., 20.),
        );
        let (triangles, surfaces) = build_world_triangles(&asset, transform, floor);
        assert_eq!(triangles.len(), 1);
        assert_eq!(surfaces, vec![17]);
        let origin = triangles[0].triangle.vertices[0];
        assert!((origin.x - 10.).abs() < 1e-4);
        assert!((origin.y - 5.).abs() < 1e-4);
        assert!((origin.z - 20.).abs() < 1e-4);
    }

    #[test]
    #[ignore = "requires a generated dropper-template catalog; set SKATE_DROPPER_TEST_DIR"]
    fn generated_templates_parse_and_are_base_centred() {
        let Ok(dir) = std::env::var("SKATE_DROPPER_TEST_DIR") else {
            return;
        };
        let templates = load_catalog(Path::new(&dir)).expect("catalog");
        assert!(!templates.is_empty());
        let verbose = std::env::var_os("SKATE_DROPPER_TEST_VERBOSE").is_some();
        for template in &templates {
            let bytes = std::fs::read(&template.file).unwrap();
            let map = SkateMap::parse_render_only(&bytes).unwrap();
            assert!(!map.geometry.vertices.is_empty(), "{}", template.name);
            assert!(!build_collision(&map).is_empty(), "{}", template.name);
            let min_y = map
                .geometry
                .vertices
                .iter()
                .map(|v| v.position[1])
                .fold(f32::INFINITY, f32::min);
            assert!(min_y.abs() < 1e-3, "{} min_y {min_y}", template.name);
            let retail_supported = map.materials[0]
                .retail_definition
                .as_deref()
                .and_then(crate::retail_render::Definition::parse)
                .map(|d| d.supported(&crate::retail_render::MaterialTuning::default()));
            assert_eq!(
                retail_supported,
                Some(true),
                "{} must use the retail object lighting path",
                template.name
            );
            if template.category == "Ramp" {
                assert!(
                    !build_drop_edges(&map).is_empty(),
                    "{} ramp must expose coping edges for drop-in",
                    template.name
                );
            }
            if verbose {
                let bad_normals = map
                    .geometry
                    .vertices
                    .iter()
                    .filter(|v| {
                        let n = v.normal;
                        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                        (len - 1.).abs() >= 0.05 && len > 1e-6
                    })
                    .count();
                let zero_normals = map
                    .geometry
                    .vertices
                    .iter()
                    .filter(|v| v.normal == [0., 0., 0.])
                    .count();
                let up = map
                    .geometry
                    .vertices
                    .iter()
                    .filter(|v| v.normal[1] > 0.9)
                    .count();
                let material = &map.materials[0];
                let drop_edges = build_drop_edges(&map).len();
                eprintln!(
                    "DROPPER_DEBUG {} category={} drop_edges={} verts={} normals_bad={} normals_zero={} normals_up={} textures={:?} retail_definition={} retail_supported={:?} alpha_mode={} color={:?}",
                    template.name,
                    template.category,
                    drop_edges,
                    map.geometry.vertices.len(),
                    bad_normals,
                    zero_normals,
                    up,
                    material.textures,
                    material.retail_definition.is_some(),
                    retail_supported,
                    material.alpha_mode,
                    material.color,
                );
            }
        }
    }

    #[test]
    fn catalog_rejects_traversal_and_accepts_valid_entries() {
        let folder = std::env::temp_dir().join(format!("sk3-dropper-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let entry = |file: &str| {
            format!(
                "{{\"id\":\"a\",\"name\":\"Bench\",\"category\":\"Bench\",\"file\":\"{file}\",\"bounds\":[[0,0,0],[1,1,1]]}}"
            )
        };
        std::fs::write(
            folder.join("catalog.json"),
            format!(
                "{{\"version\":1,\"map\":\"University\",\"templates\":[{}]}}",
                entry("a.skate")
            ),
        )
        .unwrap();
        assert_eq!(load_catalog(&folder).unwrap().len(), 1);
        std::fs::write(
            folder.join("catalog.json"),
            format!(
                "{{\"version\":1,\"map\":\"University\",\"templates\":[{}]}}",
                entry("../escape.skate")
            ),
        )
        .unwrap();
        assert!(load_catalog(&folder).is_err());
        std::fs::remove_dir_all(&folder).ok();
    }
}
