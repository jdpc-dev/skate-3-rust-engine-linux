//! Ground Sync submits edge geometry; the next PreUpdate consumes its result.
use super::ground_sync::SceneService;
use skate_core::{
    math::Vector3,
    physics::board_world::BoardWorld,
    player::offboard::ground_query::{
        self as query, ConsumeInput, Frame, GroundAdjustment, GroundQueryPacket, GroundQueryScene,
        LineHit, QueryContext,
    },
};
use skate_data::collections::Collections;
use std::sync::atomic::{AtomicUsize, Ordering};

/// TEMPORARY bounded counter for the SKATE3_DROPIN_TRACE edge-selection probe.
static EDGE_PROBE: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct State {
    pending: Option<(GroundQueryPacket, [Option<LineHit>; 7])>,
    collision_offset: f32,
}
impl State {
    pub(crate) fn load(data: &Collections) -> Result<Self, String> {
        Ok(Self {
            pending: None,
            //82C209E4 and Skate2 GetGeometryCollisionOffset82CFABD8:
            //physics_grinds layout636, full keyCBFEFFEA12F4CC27.
            collision_offset: data.float("physics_grinds", "default", "DeckCenterToTruck")?,
        })
    }
    pub(crate) fn reset(&mut self) {
        self.pending = None;
    }
    pub(crate) fn consume(&mut self, input: ConsumeInput) -> GroundAdjustment {
        let geometry = self
            .pending
            .take()
            .map(|(packet, hits)| query::interpret_hits(&packet, hits));
        query::consume_geometry(input, geometry)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit(
        &mut self,
        world: &BoardWorld,
        grind: Option<&crate::grind_world::StaticProvider>,
        host_edges: &[crate::physics::HostEdge],
        frame: [[f32; 4]; 4],
        velocity: [f32; 4],
        context: QueryContext,
        flags_2488: u32,
    ) -> Result<(), String> {
        let frame = query_frame(frame);
        let search = query::edge_search(frame, context, xyz(velocity), flags_2488);
        let candidates = SceneService { world, grind, host_edges }.edge_candidates(&search)?;
        let selected = query::select_edge(search, &candidates);
        let prepared = selected.and_then(|edge| {
            query::prepare_packet(frame, context, edge, self.collision_offset).map(|p| (edge, p))
        });
        // TEMPORARY drop-in diagnostics: bounded probe of candidate
        // availability/selection so a retail-map miss is attributable. Enable
        // with SKATE3_DROPIN_EDGE_PROBE=1 (independent of SKATE3_DROPIN_TRACE).
        if std::env::var_os("SKATE3_DROPIN_EDGE_PROBE").is_some()
            && EDGE_PROBE.fetch_add(1, Ordering::Relaxed) < 2000
        {
            let (mut min_vertical, mut min_horizontal) = (f32::INFINITY, f32::INFINITY);
            for edge in candidates.iter().take(40) {
                let c = query::closest_point(frame.position, *edge);
                let dx = c.x - frame.position.x;
                let dy = c.y - frame.position.y;
                let dz = c.z - frame.position.z;
                min_vertical = min_vertical.min(dy.abs());
                min_horizontal = min_horizontal.min((dx * dx + dz * dz).sqrt());
            }
            eprintln!(
                "DROPIN_PROBE player={:?} fwd={:?} candidates={} selected={} prepared={} min_v={min_vertical} min_h={min_horizontal}",
                frame.position,
                frame.forward,
                candidates.len(),
                selected.is_some(),
                prepared.is_some(),
            );
        }
        if let Some((edge, packet)) = prepared {
            //82C20BF0 starts the real batch during Sync. Host execution may be
            //synchronous, but interpretation/publication waits for PreUpdate.
            let hits = SceneService { world, grind, host_edges }.query_lines(&packet)?;
            // TEMPORARY drop-in diagnostics (SKATE3_DROPIN_TRACE): record the
            // ledge classification and raw seven-line results whenever the drop
            // ray sees a fall, so a Mount.IntoDropIn precondition miss is visible.
            if std::env::var_os("SKATE3_DROPIN_TRACE").is_some() {
                let geometry = query::interpret_hits(&packet, hits);
                if hits[6].is_some() {
                    eprintln!(
                        "DROPIN_EDGE kind={} edge=({:?}->{:?}) closest={:?} player={:?} fwd={:?} \
hits=[{} {} {} {} {} {} {}] f2={:?} f3={:?} f6={:?} dropY={:?} flag26={} flag27={} flag28={}",
                        geometry.kind,
                        edge.edge.start,
                        edge.edge.end,
                        edge.closest,
                        frame.position,
                        frame.forward,
                        u8::from(hits[0].is_some()),
                        u8::from(hits[1].is_some()),
                        u8::from(hits[2].is_some()),
                        u8::from(hits[3].is_some()),
                        u8::from(hits[4].is_some()),
                        u8::from(hits[5].is_some()),
                        u8::from(hits[6].is_some()),
                        hits[2].map(|h| h.fraction),
                        hits[3].map(|h| h.fraction),
                        hits[6].map(|h| h.fraction),
                        hits[6].map(|h| h.position.y),
                        geometry.flag26,
                        geometry.flag27,
                        geometry.flag28,
                    );
                }
            }
            self.pending = Some((packet, hits));
        }
        //82D321CC: no selected edge means no submit, not a fabricated query.
        Ok(())
    }
}
pub(crate) fn query_frame(frame: [[f32; 4]; 4]) -> Frame {
    Frame {
        right: xyz(frame[0]),
        up: xyz(frame[1]),
        forward: xyz(frame[2]),
        position: xyz(frame[3]),
    }
}
pub(crate) fn xyz(v: [f32; 4]) -> Vector3 {
    Vector3::new(v[0], v[1], v[2])
}
pub(crate) fn lanes(v: Vector3) -> [f32; 4] {
    [v.x, v.y, v.z, 0.]
}
pub(crate) fn native_frame(frame: Frame) -> [[f32; 4]; 4] {
    [
        lanes(frame.right),
        lanes(frame.up),
        lanes(frame.forward),
        lanes(frame.position),
    ]
}
