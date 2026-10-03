//! TEMPORARY opt-in drop-in diagnostics; not native gameplay behavior.
//! Enable with SKATE3_DROPIN_TRACE=1. Optionally bound with
//! SKATE3_DROPIN_TICKS=start:end (inclusive simulation ticks). Disabled by
//! default and observation-only: no state, flags, forces or poses are changed.
//!
//! Prints the completed physical values consumed by the stock Mount MotionGraph
//! (OffBoard+84/116/311/312, Grinds family/entry/dropping-in) together with the
//! selected MotionGraph state path, so a stand-at-coping Y press can be followed
//! from `Mount.Ground.IntoDropIn` through `OnBoard.Grinding.DroppingIn`.
use super::{GamePhysics, PlayerControls, SkaterRuntime};
use crate::graph_runtime::StockGraphs;
use skate_core::physics::board::BodyId;
use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();
static WINDOW: OnceLock<Option<(u64, u64)>> = OnceLock::new();

fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("SKATE3_DROPIN_TRACE").is_some())
}

fn in_window(tick: u64) -> bool {
    let window = WINDOW.get_or_init(|| {
        std::env::var_os("SKATE3_DROPIN_TICKS").map(|value| {
            let value = value
                .to_str()
                .expect("Drop-in tick window must be UTF-8 start:end");
            let (start, end) = value
                .split_once(':')
                .expect("Drop-in tick window must be start:end");
            let start: u64 = start.parse().expect("Invalid drop-in start tick");
            let end: u64 = end.parse().expect("Invalid drop-in end tick");
            assert!(start <= end, "Drop-in tick window is reversed");
            (start, end)
        })
    });
    match *window {
        None => true,
        Some((start, end)) => tick >= start && tick <= end,
    }
}

fn state_path(graphs: &StockGraphs, mut state: Option<usize>) -> String {
    let mut names = Vec::new();
    while let Some(id) = state {
        let node = &graphs.motion.binding.states[id];
        names.push(node.name.clone());
        state = node.parent;
    }
    names.reverse();
    names.join("/")
}

pub(super) fn stage(
    tick: u64,
    phase: &str,
    physics: &GamePhysics,
    s: &SkaterRuntime,
    controls: &PlayerControls,
    graphs: &StockGraphs,
) {
    if !enabled() || !in_window(tick) {
        return;
    }
    let p = &s.player_input.processed;
    let physical = &s.player_input.physical;
    let motion = &s.animation.motion;
    let grinds = &physical.grinds;
    let off = &physical.off_board;
    let controller = &s.animation.motion_controller;
    let omega = physics.board.bodies()[BodyId::Deck.index()]
        .rates
        .angular_velocity;
    let spin = (omega.x * omega.x + omega.y * omega.y + omega.z * omega.z).sqrt();
    let gesture = ["GestureBoth", "GestureRight", "GestureLeft"]
        .iter()
        .any(|name| motion.animation.channels.has(name));
    let channels = motion.animation.channels.names();
    let hand = s.board_possession.state.selected_hand_424;
    let hand_active = s
        .board_possession
        .state
        .hands
        .iter()
        .any(|h| h.dynamics[0][2] != 0 || h.dynamics[1][2] != 0);
    let system_on = s.skateboard_controller.fields.system_on_452;
    // Only surface frames relevant to a mount/drop-in attempt or a graph error.
    let interesting = p.grind.valid_1488
        || s.player_state.current().is_grind()
        || p.flags_2488 & 0x1000_0000 != 0
        || p.flags_2480 & 0x0008_0000 != 0
        || off.flag_330 != 0
        || off.flag_334 != 0
        || gesture
        || !channels.is_empty()
        || hand_active
        || spin > 3.0
        || controls.action_intents.contains_key("NewToggleOffBoardState")
        || controls.action_intents.contains_key("ToggleOffBoardState")
        || motion.animation.motion_intents.contains_key("OB_Mount")
        || controller.frame.last != controller.frame.current
        || !motion.errors.is_empty();
    if !interesting {
        return;
    }
    let gp = motion.gameplay_conditions;
    let board = motion.toggle_board_physical;
    let errors = if motion.errors.is_empty() {
        String::new()
    } else {
        motion.errors.join(" | ")
    };
    eprintln!(
        "DROPIN_TRACE tick={tick} phase={phase} phys={:?} cat={} flags480={:08x} flags488={:08x} \
mg={} mg_last={} \
edge={:?} obstacle={:?} loco={:?} hold={:?} free={:?} board448={} \
off_kind88={} off112={} off116={} off311={} off312={} off330={} off334={} \
        grind_words={:?} grind_family={} grind_entry={} grind_valid={} drop324={} g318={} g322={} \
ag_mount={} ag_toggle={} mg_mount={} spin={:.3} gesture={} channels={:?} hand={} hand_active={} system_on={} errors={:?}",
        s.player_state.current(),
        p.category_2512,
        p.flags_2480,
        p.flags_2488,
        state_path(graphs, controller.frame.current),
        state_path(graphs, controller.frame.last),
        gp.map(|g| g.offboard_edge_distance),
        gp.map(|g| g.offboard_obstacle_distance),
        motion.offboard_locomotion_state,
        board.map(|b| b.holding_board),
        board.map(|b| b.free_board),
        s.skateboard_controller.fields.state_448,
        off.kind_88,
        off.scalar_112,
        off.distance_116,
        off.flag_311,
        off.free_board_312,
        off.flag_330,
        off.flag_334,
        grinds.words_136_140,
        p.grind.family_1248,
        p.grind.entry_kind_1252,
        p.grind.valid_1488,
        grinds.dropping_in_324,
        grinds.flag_318,
        grinds.flag_322,
        controls.action_intents.contains_key("NewToggleOffBoardState"),
        controls.action_intents.contains_key("ToggleOffBoardState"),
        motion.animation.motion_intents.contains_key("OB_Mount"),
        spin,
        gesture,
        channels,
        hand,
        hand_active,
        system_on,
        errors,
    );
}
