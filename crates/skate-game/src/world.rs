use crate::assets::AssetManifest;
use bevy::prelude::*;

/// World transform owner. Animation only writes descendant bone transforms.
#[derive(Component)]
pub(crate) struct PlayerRoot;

pub(crate) struct WorldPlugin;
impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::srgb(0.065, 0.08, 0.10)))
            .insert_resource(GlobalAmbientLight {
                color: Color::WHITE,
                brightness: 350.,
                ..default()
            })
            .add_systems(Startup, spawn);
    }
}
fn spawn(world: &mut World) {
    let scene = {
        let server = world.resource::<AssetServer>();
        let manifest = world.resource::<AssetManifest>();
        server.load(GltfAssetLabel::Scene(0).from_asset(manifest.0.character_scene.clone()))
    };
    world.spawn((PlayerRoot, Transform::default(), Visibility::default()))
        .with_children(|parent| { parent.spawn(SceneRoot(scene)); });
    let mut prepared = crate::map_render::PreparedScene::new(world);
    let (map, root) = {
        let mut config = world.resource_mut::<crate::config::Config>();
        (config.map.take(), config.asset_root.clone())
    };
    let quality = crate::graphics_menu::scene_quality_for(&root);
    prepared.prepare_scaled(map.as_ref(), &root, quality);
    prepared.publish(world);
}

/// World-space size, in metres, of one ground texture tile. A visible repeat
/// gives the flat test course a sense of speed that plain colours cannot.
const GROUND_TEXTURE_METRES: f32 = 2.0;

/// Repeating checker texture for the test-world ground. The asset is embedded
/// so it is available even when the runtime reads an installed asset root that
/// has no source `assets/` directory.
fn ground_texture() -> Option<Image> {
    use bevy::{
        asset::RenderAssetUsages,
        image::{CompressedImageFormats, ImageAddressMode, ImageSampler, ImageSamplerDescriptor, ImageType},
    };
    let bytes = include_bytes!("../../../assets/textures/general.png");
    let mut image = Image::from_buffer(
        bytes,
        ImageType::MimeType("image/png"),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::default(),
        RenderAssetUsages::RENDER_WORLD,
    ).ok()?;
    let mut sampler = ImageSamplerDescriptor::linear();
    sampler.address_mode_u = ImageAddressMode::Repeat;
    sampler.address_mode_v = ImageAddressMode::Repeat;
    image.sampler = ImageSampler::Descriptor(sampler);
    Some(image)
}

/// Planar texture coordinates from each quad's own edges, so arbitrarily
/// oriented faces (ramps, half-pipe walls) tile at a consistent scale.
fn quad_uvs(vertices: &[skate_core::math::Vector3; 4]) -> [[f32; 2]; 4] {
    let points = vertices.map(|v| Vec3::new(v.x, v.y, v.z));
    let u_axis = (points[1] - points[0]).normalize_or_zero();
    let v_axis = (points[3] - points[0]).normalize_or_zero();
    let scale = 1.0 / GROUND_TEXTURE_METRES;
    points.map(|point| {
        let offset = point - points[0];
        [offset.dot(u_axis) * scale, offset.dot(v_axis) * scale]
    })
}

pub(crate) fn spawn_test_world(
    commands: &mut crate::map_render::SceneCommands,
    meshes: &mut impl crate::map_render::AssetSink<Mesh>,
    materials: &mut impl crate::map_render::AssetSink<StandardMaterial>,
    images: &mut impl crate::map_render::AssetSink<Image>,
) {
    let texture = ground_texture().map(|image| images.add(image));
    let colors = [
        Color::srgb(0.16, 0.19, 0.21),
        Color::srgb(0.48, 0.35, 0.22),
        Color::srgb(0.30, 0.43, 0.48),
        Color::srgb(0.24, 0.48, 0.31),
    ];
    for (quads, color) in crate::physics::ground::surfaces().into_iter().zip(colors) {
        let mut positions = Vec::new();
        let mut uvs = Vec::new();
        for vertices in quads {
            let uv = quad_uvs(&vertices);
            for i in [0, 2, 1, 0, 3, 2] {
                let v = vertices[i];
                positions.push([v.x, v.y, v.z]);
                uvs.push(uv[i]);
            }
        }
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        mesh.compute_flat_normals();
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: if texture.is_some() { Color::WHITE } else { color },
                base_color_texture: texture.clone(),
                perceptual_roughness: 0.9,
                double_sided: true,
                cull_mode: None,
                ..default()
            })),
            Transform::default(),
        ));
    }
    commands.spawn((
        DirectionalLight {
            illuminance: 11000.,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(4., 7., 4.).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}
