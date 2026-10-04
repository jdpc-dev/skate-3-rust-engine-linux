//! User-authorized ZIP spline adapter, checked against original TU3 82C1E568,
//! 82C1E6C0, 82C1EEF0 and 82C1F098 in default.patched.xex (431b8eba...).
//! Keep the native cubic payload; the game's contact primitive is its chord
//! D -> (A+B)+(C+D), including for retail cubic segments.
use skate_core::physics::grind_contact::Primitive;
use skate_data::skate_map::{Rail, SkateMap};

/// Native primitive side metadata; contact's endpoint/owner API stays unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PrimitiveMetadata {
    /// Both authored header u64s, +0 and +8. The extractor names the latter
    /// `type_signature`; preserve it verbatim, not the containing asset ID.
    pub spline_guids: [u64; 2],
    pub segment_index: u32,
    /// 82C1E568 computed near-vertical high bit; newly allocated lower bits zero.
    pub flags: u32,
}

pub(super) fn build(map: Option<&SkateMap>) -> Result<Vec<u8>, String> {
    build_rails(map.map(|map| map.rails.as_slice()).unwrap_or(&[]))
}

pub(super) fn build_rails(rails: &[Rail]) -> Result<Vec<u8>, String> {
    // TU3 82C1EEF0 reads the rail count with lhz +2, not a full 32-bit load.
    if rails.len() > usize::from(u16::MAX) {
        return Err("Pegasus spline table exceeds its uint16 rail count".into());
    }
    let mut records = Vec::new();
    for rail in rails {
        let mut header = [0u32; 6];
        let mut segments = Vec::new();
        if let Some(raw) = &rail.native {
            if raw.len() < 28 { return Err(format!("Truncated native rail {}", rail.name)); }
            // .skate is little endian; Pegasus blobs are big endian.
            let words: Vec<u32> = raw.chunks_exact(4)
                .map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect();
            header.copy_from_slice(&words[..6]);
            // u64 fields use low/high words in the package.
            header.swap(0,1); header.swap(2,3);
            if words[6] as usize != (raw.len()-28)/120 || (raw.len()-28)%120 != 0 {
                return Err(format!("Invalid native rail segment count: {}", rail.name));
            }
            for segment in words[7..].chunks_exact(30) {
                segments.push(<[u32;30]>::try_from(segment).unwrap());
            }
        } else {
            if rail.points.len() < 2 { return Err(format!("Rail {} needs two points", rail.name)); }
            let mut id = 1469598103934665603u64;
            for byte in rail.name.bytes().chain(rail.points.iter().flat_map(|p|
                p.iter().flat_map(|f| f.to_bits().to_be_bytes()))) {
                id = (id ^ byte as u64).wrapping_mul(1099511628211);
            }
            header = [(id>>32) as u32,id as u32,0x2c701707,0x0007004a,0,0];
            let mut points = rail.points.clone();
            let first = points[0]; let last = *points.last().unwrap();
            // Closed authored polylines retain their final edge, even when short.
            if rail.closed && first != last { points.push(first); }
            for pair in points.windows(2) {
                let [start,end] = [pair[0],pair[1]];
                let mut segment = [0u32;30];
                for i in 0..3 {
                    let delta = end[i]-start[i];
                    segment[i] = (-2.*delta).to_bits();
                    segment[4+i] = (3.*delta).to_bits();
                    segment[12+i] = start[i].to_bits();
                    segment[20+i] = start[i].min(end[i]).to_bits();
                    segment[24+i] = start[i].max(end[i]).to_bits();
                }
                segment[15] = 1f32.to_bits();
                segments.push(segment);
            }
        }
        if segments.is_empty() { return Err(format!("Rail {} has no segments",rail.name)); }
        for s in &segments {
            if s[..28].iter().any(|w| !f32::from_bits(*w).is_finite()) {
                return Err(format!("Non-finite spline data in {}",rail.name));
            }
            if (0..3).any(|i| f32::from_bits(s[20+i]) > f32::from_bits(s[24+i])) {
                return Err(format!("Inverted spline bounds in {}", rail.name));
            }
        }
        records.push((header,segments));
    }
    let count = records.iter().try_fold(0usize, |count, r| count.checked_add(r.1.len()))
        .ok_or("Spline segment count overflow")?;
    let base = 16+records.len()*32;
    let size = count.checked_mul(144).and_then(|size| size.checked_add(base))
        .filter(|&size| u32::try_from(size).is_ok()).ok_or("Spline blob exceeds uint32 offsets")?;
    let mut bytes = vec![0; size];
    put(&mut bytes,0,records.len() as u32); put(&mut bytes,4,count as u32);
    put(&mut bytes,8,16); put(&mut bytes,12,base as u32);
    let mut at = base;
    for (index,(header,segments)) in records.iter().enumerate() {
        let r = 16+index*32;
        for i in 0..5 { put(&mut bytes,r+i*4,header[i]); }
        put(&mut bytes,r+20,at as u32);
        put(&mut bytes,r+24,(at+(segments.len()-1)*144) as u32);
        put(&mut bytes,r+28,header[5]);
        for (i,segment) in segments.iter().enumerate() {
            for (j,w) in segment.iter().enumerate() { put(&mut bytes,at+j*4,*w); }
            put(&mut bytes,at+120,r as u32);
            if i>0 { put(&mut bytes,at+124,(at-144) as u32); }
            if i+1<segments.len() { put(&mut bytes,at+128,(at+144) as u32); }
            at+=144;
        }
    }
    Ok(bytes)
}

pub(crate) fn primitives(map: Option<&SkateMap>) -> Result<Vec<Primitive>,String> {
    let bytes = build(map)?;
    primitives_from_blob(&bytes)
}

// Only accepts a blob returned by build/build_rails; public package input is
// validated there. Owner is a map-local header handle, not the repeating ID.
pub(super) fn primitives_from_blob(bytes: &[u8]) -> Result<Vec<Primitive>,String> {
    decoded_from_blob(bytes).map(|(primitives, _)| primitives)
}

/// Polyline points approximating one authored cubic within one centimetre.
/// A straight segment returns exactly its two endpoints, so native straight
/// rails keep the original single-chord primitive. Curved handrail tips (the
/// swan neck / lower curl) are the only segments that gain sub-chords, which
/// keeps the grind surface on the visible tube instead of cutting through it.
pub(crate) fn sub_chord_points(coefficients: &[[f32;4];4]) -> Vec<[f32;4]> {
    let [a, b, c, d] = *coefficients;
    let position = |t: f32| -> [f32;4] {
        std::array::from_fn(|j| a[j].mul_add(t, b[j]).mul_add(t, c[j]).mul_add(t, d[j]))
    };
    let start = position(0.0);
    let end = position(1.0);
    let chord = sub4(end, start);
    let length = dot3(chord).sqrt();
    let mut divisions = 1usize;
    if length > 1.0e-6 {
        loop {
            let unit = scale4(chord, 1.0 / length);
            let samples = divisions * 2;
            let mut deviation = 0.0f32;
            for k in 1..samples {
                let t = k as f32 / samples as f32;
                let rel = sub4(position(t), start);
                let along = dot4(rel, unit);
                let perpendicular = sub4(rel, scale4(unit, along));
                deviation = deviation.max(dot4(perpendicular, perpendicular).sqrt());
            }
            if deviation <= 0.01 || divisions >= 16 {
                break;
            }
            divisions *= 2;
        }
    }
    if divisions == 1 {
        return vec![start, end];
    }
    (0..=divisions)
        .map(|k| position(k as f32 / divisions as f32))
        .collect()
}

fn sub4(a: [f32;4], b: [f32;4]) -> [f32;4] { std::array::from_fn(|i| a[i]-b[i]) }
fn scale4(a: [f32;4], s: f32) -> [f32;4] { a.map(|v| v*s) }
fn dot4(a: [f32;4], b: [f32;4]) -> f32 { a[0]*b[0]+a[1]*b[1]+a[2]*b[2]+a[3]*b[3] }
fn dot3(a: [f32;4]) -> f32 { a[0]*a[0]+a[1]*a[1]+a[2]*a[2] }

pub(super) fn decoded_from_blob(bytes: &[u8]) -> Result<(Vec<Primitive>, Vec<PrimitiveMetadata>),String> {
    let word = |at| u32::from_be_bytes(bytes[at..at+4].try_into().unwrap());
    let mut result = Vec::new();
    let mut metadata = Vec::new();
    let base = word(12) as usize;
    let segment_base = |r: usize| word(r+20) as usize;
    for i in 0..word(4) as usize {
        let s = base+i*144;
        let r = word(s+120) as usize;
        let coefficients: [[f32;4];4] = std::array::from_fn(|v|
            std::array::from_fn(|j| f32::from_bits(word(s+v*16+j*4))));
        let points = sub_chord_points(&coefficients);
        let owner = ((r-16)/32 + 1) as u64;
        let segment_index = ((s-segment_base(r))/144) as u32;
        let spline_guids = [((word(r) as u64)<<32)|word(r+4) as u64,
            ((word(r+8) as u64)<<32)|word(r+12) as u64];
        let horizontal_tolerance = f32::from_bits(0x3780_0000);
        for pair in points.windows(2) {
            let start = pair[0];
            let end = pair[1];
            let delta = [end[0]-start[0], end[1]-start[1], end[2]-start[2]];
            if !(delta[0].is_finite() && delta[1].is_finite() && delta[2].is_finite()) {
                return Err(format!("Spline segment {i} has a non-finite contact chord"));
            }
            // Original 82C1E568 / 82C1F098 retain every authored knot. Only
            // the straight contact chord is subdivided; admission is unchanged.
            let vertical = delta[0].abs() <= horizontal_tolerance
                && delta[2].abs() <= horizontal_tolerance;
            result.push(Primitive { start, end, owner });
            metadata.push(PrimitiveMetadata {
                spline_guids,
                segment_index,
                flags: if vertical { 0x8000_0000 } else { 0 },
            });
        }
    }
    Ok((result, metadata))
}
fn put(bytes:&mut [u8],at:usize,w:u32) { bytes[at..at+4].copy_from_slice(&w.to_be_bytes()); }
