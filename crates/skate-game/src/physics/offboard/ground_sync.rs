//! Host side scene service for the recovered BipedGround synchronizer.
//!
//! The synchronizer owns the query ordering in `skate-core`; this adapter only
//! exposes the already loaded BoardWorld.  It does not cache contacts or run a
//! second solver.  Every query therefore sees the same authored triangles and
//! packed surfaces as the board collision pass.
use super::ground_query::{self, Bounds, EdgeBody, IndexedEdgeBody, PrimaryEdges, Segment};
use skate_core::{
    math::Vector3,
    physics::board_world::BoardWorld,
    player::offboard::ground_query::{
        Edge, EdgeSearch, Frame, GroundQueryPacket, GroundQueryScene, LineHit,
    },
};

pub(crate) struct SceneService<'a> {
    pub world: &'a BoardWorld,
    /// Loaded authored spline segments. Retail maps carry no
    /// QueryMetadata.static_edges, so these are the biped ledge candidates.
    pub grind: Option<&'a crate::grind_world::StaticProvider>,
    /// Coping/ledge edges of host-placed objects (e.g. dropped ramps).
    pub host_edges: &'a [crate::physics::HostEdge],
}

impl GroundQueryScene for SceneService<'_> {
    type Error = &'static str;

    fn edge_candidates(&mut self, search: &EdgeSearch) -> Result<Vec<Edge>, Self::Error> {
        // Retail maps author no static edges, so the biped ground query would
        // otherwise never see a coping. Feed the grind spline segments (the
        // same authored asset native's edge collection reads), bounded by the
        // provider octree.
        let mut segments: Vec<Segment> = match self.grind {
            Some(provider) => provider
                .query(
                    [search.min.x, search.min.y, search.min.z],
                    [search.max.x, search.max.y, search.max.z],
                )
                .map_err(|_| "Grind edge query failed")?
                .into_iter()
                .filter_map(|index| provider.primitives().get(index))
                .map(|p| {
                    let start = Vector3::new(p.start[0], p.start[1], p.start[2]);
                    let end = Vector3::new(p.end[0], p.end[1], p.end[2]);
                    Segment {
                        edge: Edge { start, end },
                        local_bounds: Bounds {
                            min: Vector3::new(
                                start.x.min(end.x),
                                start.y.min(end.y),
                                start.z.min(end.z),
                            ),
                            max: Vector3::new(
                                start.x.max(end.x),
                                start.y.max(end.y),
                                start.z.max(end.z),
                            ),
                        },
                    }
                })
                .collect(),
            None => Vec::new(),
        };
        // Host-placed copings are world-space segments, bounded by the same
        // search as the authored splines.
        for host in self.host_edges {
            let start = Vector3::new(host.start[0], host.start[1], host.start[2]);
            let end = Vector3::new(host.end[0], host.end[1], host.end[2]);
            let local_bounds = Bounds {
                min: Vector3::new(
                    start.x.min(end.x),
                    start.y.min(end.y),
                    start.z.min(end.z),
                ),
                max: Vector3::new(
                    start.x.max(end.x),
                    start.y.max(end.y),
                    start.z.max(end.z),
                ),
            };
            let search_bounds = Bounds {
                min: Vector3::new(search.min.x, search.min.y, search.min.z),
                max: Vector3::new(search.max.x, search.max.y, search.max.z),
            };
            let overlaps = !(local_bounds.max.x < search_bounds.min.x
                || local_bounds.min.x > search_bounds.max.x
                || local_bounds.max.y < search_bounds.min.y
                || local_bounds.min.y > search_bounds.max.y
                || local_bounds.max.z < search_bounds.min.z
                || local_bounds.min.z > search_bounds.max.z);
            if overlaps {
                segments.push(Segment {
                    edge: Edge { start, end },
                    local_bounds,
                });
            }
        }
        let bounds = segments
            .iter()
            .map(|segment| segment.local_bounds)
            .reduce(|b, s| Bounds {
                min: Vector3::new(
                    b.min.x.min(s.min.x),
                    b.min.y.min(s.min.y),
                    b.min.z.min(s.min.z),
                ),
                max: Vector3::new(
                    b.max.x.max(s.max.x),
                    b.max.y.max(s.max.y),
                    b.max.z.max(s.max.z),
                ),
            });
        let body = EdgeBody {
            local_to_world: Frame::IDENTITY,
            local_bounds: bounds.unwrap_or(Bounds {
                min: Vector3::ZERO,
                max: Vector3::ZERO,
            }),
            segments: &segments,
        };
        // Indexed bodies are queried without the dynamic provider's origin
        // distance gate; segments are already world space.
        let indexed = [IndexedEdgeBody {
            id: 0,
            disabled: false,
            body: &body,
        }];
        ground_query::with_world_scene(
            self.world,
            PrimaryEdges::Normal {
                dynamic: &[],
                vehicles: &[],
            },
            &indexed,
            |scene| scene.edge_candidates(search),
        )
    }

    fn query_lines(
        &mut self,
        packet: &GroundQueryPacket,
    ) -> Result<[Option<LineHit>; 7], Self::Error> {
        ground_query::with_world_scene(
            self.world,
            PrimaryEdges::Normal {
                dynamic: &[],
                vehicles: &[],
            },
            &[] as &[IndexedEdgeBody<'_>],
            |scene| scene.query_lines(packet),
        )
    }
}
