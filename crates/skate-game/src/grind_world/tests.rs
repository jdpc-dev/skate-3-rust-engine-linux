use super::*;
use skate_data::skate_map::Rail;

fn native(id: u64, length: f32) -> Rail {
    let mut raw = Vec::new();
    raw.extend_from_slice(&id.to_le_bytes());
    raw.extend_from_slice(&0x2c70_1707_0006_0230u64.to_le_bytes());
    for word in [0u32, 0, 1] { raw.extend_from_slice(&word.to_le_bytes()); }
    let mut segment = [0u32; 30];
    segment[0] = (-2.*length).to_bits();
    segment[4] = (3.*length).to_bits();
    segment[15] = 1f32.to_bits();
    segment[24] = length.to_bits();
    for word in segment { raw.extend_from_slice(&word.to_le_bytes()); }
    Rail { name: "owned test rail".into(), closed: false, points: vec![], native: Some(raw) }
}

/// Diagnostic: distinct native rail type signatures, with curvature counts.
#[test]
#[ignore = "diagnostic; requires SKATE_MAP_TEST_PATH"]
fn dump_rail_type_signatures() {
    let map = skate_data::skate_map::SkateMap::load(std::path::Path::new(
        &std::env::var("SKATE_MAP_TEST_PATH").unwrap(),
    ))
    .unwrap();
    let mut by_type = std::collections::BTreeMap::<u64, (usize, usize)>::new();
    for rail in &map.rails {
        let Some(raw) = &rail.native else { continue };
        if raw.len() < 16 {
            continue;
        }
        let signature = u64::from_le_bytes(raw[8..16].try_into().unwrap());
        let words: Vec<u32> = raw
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
            .collect();
        let mut curved = 0;
        for segment in words[7..].chunks_exact(30) {
            let f = |i: usize| f32::from_bits(segment[i]);
            let position = |t: f32| {
                let a = [f(0), f(1), f(2)];
                let b = [f(4), f(5), f(6)];
                let c = [f(8), f(9), f(10)];
                let d = [f(12), f(13), f(14)];
                core::array::from_fn::<f32, 3, _>(|j| ((a[j] * t + b[j]) * t + c[j]) * t + d[j])
            };
            let start = position(0.0);
            let end = position(1.0);
            let mid = position(0.5);
            let midpoint = core::array::from_fn::<f32, 3, _>(|j| (start[j] + end[j]) * 0.5);
            let dev = ((mid[0] - midpoint[0]).powi(2)
                + (mid[1] - midpoint[1]).powi(2)
                + (mid[2] - midpoint[2]).powi(2))
            .sqrt();
            if dev > 0.01 {
                curved += 1;
            }
        }
        let entry = by_type.entry(signature).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += curved;
    }
    for (signature, (rails, curved)) in &by_type {
        eprintln!("RAIL_TYPE sig=0x{signature:016x} rails={rails} curved_segments={curved}");
    }
}

/// Diagnostic: the real retail provider builds and stays index-aligned after
/// curved-segment tessellation.
#[test]
#[ignore = "diagnostic; requires SKATE_MAP_TEST_PATH"]
fn retail_provider_builds_with_curved_tessellation() {
    let map = skate_data::skate_map::SkateMap::load(std::path::Path::new(
        &std::env::var("SKATE_MAP_TEST_PATH").unwrap(),
    ))
    .unwrap();
    let provider = super::StaticProvider::new(Some(&map)).unwrap();
    let primitives = provider.primitives();
    assert!(primitives.len() >= map.rails.len());
    for i in 0..primitives.len() {
        assert!(provider.metadata(i).is_some(), "missing metadata {i}");
        assert!(provider.source(i).is_some(), "missing source {i}");
        assert!(provider.source_rail_index(i).is_some(), "missing rail {i}");
    }
    eprintln!(
        "RETAIL_PROVIDER primitives={} native_segments=27008 rails={}",
        primitives.len(),
        map.rails.len()
    );
}

/// Diagnostic: how far each native cubic strays from its straight chord.
#[test]
#[ignore = "diagnostic; requires SKATE_MAP_TEST_PATH"]
fn curved_rail_chord_deviation() {
    let map = skate_data::skate_map::SkateMap::load(std::path::Path::new(
        &std::env::var("SKATE_MAP_TEST_PATH").unwrap(),
    ))
    .unwrap();
    let (mut segments, mut curved, mut worst) = (0usize, 0usize, 0.0f32);
    let mut histogram = [0usize; 7];
    for rail in &map.rails {
        let Some(raw) = &rail.native else { continue };
        if raw.len() < 28 {
            continue;
        }
        let words: Vec<u32> = raw
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
            .collect();
        for segment in words[7..].chunks_exact(30) {
            let f = |i: usize| f32::from_bits(segment[i]);
            let a = [f(0), f(1), f(2)];
            let b = [f(4), f(5), f(6)];
            let c = [f(8), f(9), f(10)];
            let d = [f(12), f(13), f(14)];
            let position = |t: f32| {
                core::array::from_fn::<f32, 3, _>(|j| ((a[j] * t + b[j]) * t + c[j]) * t + d[j])
            };
            let start = position(0.0);
            let end = position(1.0);
            let chord = core::array::from_fn::<f32, 3, _>(|j| end[j] - start[j]);
            let chord_length = (chord[0] * chord[0] + chord[1] * chord[1] + chord[2] * chord[2]).sqrt();
            let mut deviation = 0.0f32;
            if chord_length > 1.0e-6 {
                let unit = core::array::from_fn::<f32, 3, _>(|j| chord[j] / chord_length);
                for k in 1..16 {
                    let t = k as f32 / 16.0;
                    let p = position(t);
                    let rel = core::array::from_fn::<f32, 3, _>(|j| p[j] - start[j]);
                    let along = rel[0] * unit[0] + rel[1] * unit[1] + rel[2] * unit[2];
                    let perp = core::array::from_fn::<f32, 3, _>(|j| rel[j] - unit[j] * along);
                    deviation = deviation.max(
                        (perp[0] * perp[0] + perp[1] * perp[1] + perp[2] * perp[2]).sqrt(),
                    );
                }
            }
            segments += 1;
            if deviation > 0.01 {
                curved += 1;
            }
            worst = worst.max(deviation);
            let bucket = (deviation * 100.0).log2().max(0.0) as usize;
            histogram[bucket.min(6)] += 1;
            if deviation > 0.1 {
                eprintln!(
                    "CURVED_SEG rail={} deviation={deviation:.3} start=({:.3},{:.3},{:.3}) end=({:.3},{:.3},{:.3})",
                    rail.name, start[0], start[1], start[2], end[0], end[1], end[2]
                );
            }
        }
    }
    eprintln!(
        "CURVED_RAIL segments={segments} curved(>1cm)={curved} worst={worst:.4} histogram_cm={histogram:?}"
    );
}

#[test]
fn no_map_keeps_current_flat_course_empty() {
    assert!(primitives(None).unwrap().is_empty());
    let provider = StaticProvider::new(None).unwrap();
    assert!(provider.query([-1.; 3], [1.; 3]).unwrap().is_empty());
}

#[test]
fn native_tiny_and_duplicate_knots_are_not_dropped() {
    let bytes = spline::build_rails(&[native(1, 0.), native(2, 0.0001)]).unwrap();
    let (entries, metadata) = spline::decoded_from_blob(&bytes).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].start, entries[0].end);
    assert_eq!(metadata[0].flags & 0x8000_0000, 0x8000_0000);
    assert!(entries[1].end[0] > 0.);
    assert_eq!(metadata[1].segment_index, 0);
}

#[test]
fn same_native_spline_id_does_not_alias_header_instances() {
    let bytes = spline::build_rails(&[native(0x1234, 1.), native(0x1234, 2.)]).unwrap();
    let (entries, metadata) = spline::decoded_from_blob(&bytes).unwrap();
    assert_eq!(metadata[0].spline_guids, metadata[1].spline_guids);
    assert_eq!(metadata[0].spline_guids, [0x1234, 0x2c70_1707_0006_0230]);
    assert_ne!(entries[0].owner, entries[1].owner);
}

#[test]
fn provider_keeps_both_header_guids_and_source_rail_identity() {
    use skate_data::skate_map::{Extension, Geometry, SkateMap};
    // Synthetic numeric records exercise the stock package layout without
    // embedding private stock bytes. The second GUID is not an asset ID.
    let mut rails = [native(0x1234, 1.), native(0x1234, 2.)];
    let second_guid = 0x1020_3040_5060_7080u64;
    rails[1].native.as_mut().unwrap()[8..16].copy_from_slice(&second_guid.to_le_bytes());
    let records: Vec<_> = rails.iter_mut().enumerate().map(|(index, rail)| {
        rail.name = format!("0xAA_9_{}", index+7);
        serde_json::json!({
            "stream_file": "synthetic.xsf", "asset_id": "0xAA",
            "section_index": 9, "section_offset": 128, "rail_index": index+7,
            "spline_id": "0x1234", "type_signature": if index == 0 {
                "0x2c70170700060230".to_owned()
            } else { format!("0x{second_guid:016x}") },
            "segment_count": 1, "flags": 0, "trailing_word": 0, "closed": false
        })
    }).collect();
    let map = SkateMap {
        version: 14, name: "metadata fixture".into(), spawn: [0.; 3], heading: 0.,
        environment: vec![], materials: vec![], textures: vec![],
        geometry: Geometry { vertices: vec![], indices: vec![], collision: vec![] },
        rails: rails.into(), doors: vec![], lights: vec![], routes: vec![],
        extensions: vec![Extension { tag: *b"WMET", schema: 1,
            payload: serde_json::to_vec(&serde_json::json!({
                "grind_coordinate_policy": { "mode": "world_space" },
                "grind_splines": records
            })).unwrap() }],
    };
    let provider = StaticProvider::new(Some(&map)).unwrap();
    let entries = provider.primitives();
    assert_eq!(provider.metadata(1).unwrap().spline_guids, [0x1234, second_guid]);
    assert_eq!(provider.spline_guids(entries[1].owner), Some([0x1234, second_guid]));
    assert_ne!(provider.spline_guids(entries[0].owner), provider.spline_guids(entries[1].owner));
    assert_eq!(provider.source_rail_index(0), Some(7));
    assert_eq!(provider.source_rail_index(1), Some(8));
    assert_eq!(provider.source(1).unwrap().asset_id, "0xAA");
    assert_eq!(provider.metadata(1).unwrap().flags, 0);
    assert_eq!(provider.spline_guids(0), None);
    assert_eq!(provider.spline_guids(u64::MAX), None);
    assert!(provider.metadata(2).is_none());
}

#[test]
fn vertical_flag_uses_horizontal_tolerance_not_chord_length() {
    let epsilon = f32::from_bits(0x3780_0000);
    let bytes = spline::build_rails(&[native(1, epsilon), native(2, epsilon*2.)]).unwrap();
    let (_, metadata) = spline::decoded_from_blob(&bytes).unwrap();
    assert_eq!(metadata[0].flags, 0x8000_0000);
    assert_eq!(metadata[1].flags, 0);
}

#[test]
fn native_payload_and_inclusive_link_bounds_survive_conversion() {
    let rail = native(0x1234_5678_9abc_def0, 1.);
    let bytes = spline::build_rails(&[rail]).unwrap();
    let word = |offset| u32::from_be_bytes(bytes[offset..offset+4].try_into().unwrap());
    assert_eq!(word(16), 0x1234_5678);
    assert_eq!(word(20), 0x9abc_def0);
    assert_eq!(word(36), 48);
    assert_eq!(word(40), 48);
    assert_eq!(word(48+120), 16);
    assert_eq!(word(48+124), 0);
    assert_eq!(word(48+128), 0);
}

#[test]
fn octree_resident_head_insertion_and_forty_cap() {
    use super::octree::{Bounds, Octree};
    let b = Bounds { min: [-1.; 3], max: [1.; 3] };
    let tree = Octree::new(b, vec![b; 50]).unwrap();
    assert_eq!(tree.query(b, 40), (10..50).rev().collect::<Vec<_>>());
    assert_eq!(tree.query(b, 0), Vec::<usize>::new());
}

#[test]
fn identity_bounds_transform_retains_native_center_rounding() {
    use super::octree::Bounds;
    let authored = Bounds { min: [16_777_216.; 3], max: [16_777_218.; 3] };
    let query_bounds = authored.identity_transformed();
    assert_eq!(query_bounds.min, [16_777_215.; 3]);
    assert_eq!(query_bounds.max, [16_777_216.; 3]);
}

#[test]
fn native_split_redistributes_old_linked_list_before_next_insertion() {
    use super::octree::{Bounds, Octree};
    let b = Bounds { min: [-1.; 3], max: [1.; 3] };
    let point = Bounds { min: [0.; 3], max: [0.; 3] };
    // Fourth movable entry causes one split; fifth causes a second. Each
    // redistribution walks the old head and prepends, reversing that list.
    let tree = Octree::new(b, vec![point; 5]).unwrap();
    assert_eq!(tree.query(b, 40), [3, 2, 1, 0, 4]);
}

#[test]
fn malformed_native_counts_and_inverted_authored_bounds_are_errors() {
    let mut rail = native(1, 1.);
    rail.native.as_mut().unwrap()[24..28].copy_from_slice(&2u32.to_le_bytes());
    assert!(spline::build_rails(&[rail]).is_err());
    let mut rail = native(1, 1.);
    rail.native.as_mut().unwrap()[28+80..28+84].copy_from_slice(&2f32.to_le_bytes());
    assert!(spline::build_rails(&[rail]).is_err());
}
