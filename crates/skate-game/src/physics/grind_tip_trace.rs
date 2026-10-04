//! TEMPORARY opt-in grind-endpoint diagnostics; not native gameplay behavior.
//! Enable with SKATE3_GRIND_TIP_TRACE=1. Optionally bound with
//! SKATE3_GRIND_TIP_TICKS=start:end (inclusive simulation ticks) and
//! SKATE3_GRIND_TIP_RADIUS=<metres> (default 0.75). Disabled by default and
//! observation-only: no state, flags, forces or poses are changed.
//!
//! Prints the board pose/velocity, the nearest grind primitive endpoint, the
//! active investigation fields, the real world contact normal/flags and the
//! wipeout requests whenever the board is near the end of a rail. This is the
//! Fase 1 diagnostic for the rail-tip collision report: it distinguishes an
//! end-cap/post contact (normal parallel to the rail axis), an endpoint pin
//! (overshoot past the end) and an acquisition rejection (proximity without a
//! valid candidate).
use super::{GamePhysics, SkaterRuntime};
use skate_core::{physics::board::BodyId, player::state::PhysicalStateId};
use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();
static WINDOW: OnceLock<Option<(u64, u64)>> = OnceLock::new();
static RADIUS: OnceLock<f32> = OnceLock::new();

pub(super) fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("SKATE3_GRIND_TIP_TRACE").is_some())
}

fn window() -> Option<(u64, u64)> {
    *WINDOW.get_or_init(|| {
        std::env::var_os("SKATE3_GRIND_TIP_TICKS").map(|value| {
            let value = value
                .to_str()
                .expect("Grind tip tick window must be UTF-8 start:end");
            let (start, end) = value
                .split_once(':')
                .expect("Grind tip tick window must be start:end");
            let start: u64 = start.parse().expect("Invalid grind tip start tick");
            let end: u64 = end.parse().expect("Invalid grind tip end tick");
            assert!(start <= end, "Grind tip tick window is reversed");
            (start, end)
        })
    })
}

fn in_window(tick: u64) -> bool {
    match window() {
        None => true,
        Some((start, end)) => tick >= start && tick <= end,
    }
}

/// One startup confirmation so a run proves the opt-in was actually seen.
pub(super) fn announce() {
    if !enabled() {
        return;
    }
    eprintln!(
        "GRIND_TIP_TRACE enabled radius={:.2} window={:?}",
        radius(),
        window()
    );
}

fn radius() -> f32 {
    *RADIUS.get_or_init(|| {
        std::env::var("SKATE3_GRIND_TIP_RADIUS")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|value: &f32| value.is_finite() && *value > 0.0)
            .unwrap_or(0.75)
    })
}

struct Endpoint {
    index: usize,
    owner: u64,
    start: [f32; 3],
    end: [f32; 3],
    nearest: u8,
    distance: f32,
    t: f32,
    overshoot_start: f32,
    overshoot_end: f32,
    lateral: f32,
    axis: [f32; 3],
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    norm(sub(a, b))
}

/// Nearest primitive with a query box bounded to the board's neighbourhood.
fn nearest_endpoint(physics: &GamePhysics, board: [f32; 3]) -> Option<Endpoint> {
    let span = 2.0f32;
    let min = [board[0] - span, board[1] - span, board[2] - span];
    let max = [board[0] + span, board[1] + span, board[2] + span];
    let indices = physics.grind_world.query(min, max).ok()?;
    let primitives = physics.grind_world.primitives();
    let mut best: Option<Endpoint> = None;
    for &index in &indices {
        let Some(primitive) = primitives.get(index) else {
            continue;
        };
        let start = [primitive.start[0], primitive.start[1], primitive.start[2]];
        let end = [primitive.end[0], primitive.end[1], primitive.end[2]];
        let dist_start = distance(board, start);
        let dist_end = distance(board, end);
        let (nearest, endpoint_distance) = if dist_start <= dist_end {
            (0u8, dist_start)
        } else {
            (1u8, dist_end)
        };
        if best.as_ref().is_none_or(|b| endpoint_distance < b.distance) {
            let axis = sub(end, start);
            let length = norm(axis);
            let (t, overshoot_start, overshoot_end, lateral, axis) = if length > 1e-6 {
                let unit = scale(axis, 1.0 / length);
                let rel = sub(board, start);
                let t = dot(rel, unit);
                (t, -t, t - length, norm(sub(rel, scale(unit, t))), unit)
            } else {
                (0.0, 0.0, 0.0, endpoint_distance, [0.0, 0.0, 0.0])
            };
            best = Some(Endpoint {
                index,
                owner: primitive.owner,
                start,
                end,
                nearest,
                distance: endpoint_distance,
                t,
                overshoot_start,
                overshoot_end,
                lateral,
                axis,
            });
        }
    }
    best
}

/// List of the actual wipeout request reasons held this frame.
fn wipeout_reasons(skater: &SkaterRuntime) -> Vec<usize> {
    skater
        .wipeout
        .state
        .reasons
        .iter()
        .enumerate()
        .filter_map(|(index, &set)| set.then_some(index))
        .collect()
}

/// Log the engagement wipeout reasons before selection clears them.
pub(super) fn note_wipeout_reasons(tick: u64, reasons: &[usize]) {
    if !enabled() || !in_window(tick) || reasons.is_empty() {
        return;
    }
    eprintln!("GRIND_TIP_WIPEOUT tick={tick} reasons={reasons:?}");
}

pub(super) fn stage(tick: u64, phase: &str, physics: &GamePhysics, skater: &SkaterRuntime) {
    if !enabled() || !in_window(tick) {
        return;
    }
    let deck = physics.board.part_transforms()[BodyId::Deck.index()];
    let board = [deck.translation.x, deck.translation.y, deck.translation.z];
    let Some(endpoint) = nearest_endpoint(physics, board) else {
        return;
    };
    let p = &skater.player_input.processed;
    let ground = &physics.riding.ground;
    let state = skater.player_state.current();
    let active = state.is_grind() || state == PhysicalStateId::Nonspecific;
    let reasons = wipeout_reasons(skater);
    let interesting = endpoint.distance < radius()
        && (active
            || p.grind.valid_1488
            || ground.part_contact_count != 0
            || !reasons.is_empty()
            || endpoint.overshoot_end > 0.0
            || endpoint.overshoot_start > 0.0);
    if !interesting {
        return;
    }

    let board_velocity = physics.board.bodies()[BodyId::Deck.index()]
        .rates
        .linear_velocity;
    let board_velocity = [board_velocity.x, board_velocity.y, board_velocity.z];
    let requested = [
        f32::from_bits(p.vectors_400_416[0][0]),
        f32::from_bits(p.vectors_400_416[0][1]),
        f32::from_bits(p.vectors_400_416[0][2]),
    ];
    let up = [
        deck.basis.columns[1][0],
        deck.basis.columns[1][1],
        deck.basis.columns[1][2],
    ];
    let forward = [
        deck.basis.columns[2][0],
        deck.basis.columns[2][1],
        deck.basis.columns[2][2],
    ];
    let g = &p.grind;
    let bit = f32::from_bits;
    let point = [bit(g.point_1120[0]), bit(g.point_1120[1]), bit(g.point_1120[2])];
    let g_start = [
        bit(g.primitive_start_1264[0]),
        bit(g.primitive_start_1264[1]),
        bit(g.primitive_start_1264[2]),
    ];
    let g_end = [
        bit(g.primitive_end_1280[0]),
        bit(g.primitive_end_1280[1]),
        bit(g.primitive_end_1280[2]),
    ];
    let proximity = [
        bit(g.vector_1232[0]),
        bit(g.vector_1232[1]),
        bit(g.vector_1232[2]),
    ];
    let second_start = [
        bit(g.second_start_1312[0]),
        bit(g.second_start_1312[1]),
        bit(g.second_start_1312[2]),
    ];
    let second_end = [
        bit(g.second_end_1328[0]),
        bit(g.second_end_1328[1]),
        bit(g.second_end_1328[2]),
    ];
    let overall = ground.overall_normal;
    let overall = [overall.x, overall.y, overall.z];
    let normal_parallel = dot(overall, endpoint.axis);
    let closing = ground.closing_velocity;
    let closing = [closing.x, closing.y, closing.z];
    let parts: Vec<(bool, [f32; 3])> = ground
        .parts
        .iter()
        .map(|part| {
            (
                part.in_contact,
                [part.normal.x, part.normal.y, part.normal.z],
            )
        })
        .collect();
    let pose_error = [
        skater.collision_pose_error[0],
        skater.collision_pose_error[1],
        skater.collision_pose_error[2],
    ];
    let selector_valid = skater.trajectory.selector.valid();
    let selector_grind = skater
        .trajectory
        .selector
        .selection()
        .and_then(|selection| selection.grind)
        .map(|target| [
            [target.edge.start[0], target.edge.start[1], target.edge.start[2]],
            [target.edge.end[0], target.edge.end[1], target.edge.end[2]],
        ]);

    eprintln!(
        "GRIND_TIP tick={tick:?} phase={phase:?} state={state:?} category={category} valid={valid} \
family={family} entry_kind={entry_kind} gk={gk} gf={gf:08x} f1516={f1516:08x} impact={impact:.3} \
point={point:?} prim_start={g_start:?} prim_end={g_end:?} prox={proximity:?} \
second_start={second_start:?} second_end={second_end:?} \
board={board:?} deck_vel={board_velocity:?} requested_vel={requested:?} up={up:?} fwd={forward:?} \
nearest_index={nearest_index} owner={owner} which={which} dist={distance:.3} ds={ds:.3} de={de:.3} \
t={t:.3} over_s={over_s:.3} over_e={over_e:.3} lateral={lateral:.3} prim_axis={axis:?} \
contact_flags={contact_flags:08x} parts_contact={parts_contact} wheels_contact={wheels_contact} \
overall={overall:?} normal_parallel={normal_parallel:.3} closing={closing:?} parts_n={parts:?} \
pose_err={pose_error:?} max_err={max_err:?} reasons={reasons:?} reason_count={reason_count} mode={mode} \
sel_valid={selector_valid} sel_grind={selector_grind:?}",
        category = p.category_2512,
        valid = u8::from(g.valid_1488),
        family = g.family_1248,
        entry_kind = g.entry_kind_1252,
        gk = g.geometry_kind_1464,
        gf = g.geometry_flags_1476,
        f1516 = g.flags_1516,
        impact = g.impact_speed_1492,
        nearest_index = endpoint.index,
        owner = endpoint.owner,
        which = endpoint.nearest,
        distance = endpoint.distance,
        ds = distance(board, endpoint.start),
        de = distance(board, endpoint.end),
        t = endpoint.t,
        over_s = endpoint.overshoot_start,
        over_e = endpoint.overshoot_end,
        lateral = endpoint.lateral,
        axis = endpoint.axis,
        contact_flags = ground.collision_flags,
        parts_contact = ground.part_contact_count,
        wheels_contact = ground.wheel_contact_count,
        max_err = skater.collision_maximum_error,
        reason_count = skater.wipeout.state.count,
        mode = skater.wipeout.state.mode,
    );
}
