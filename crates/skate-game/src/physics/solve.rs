//! One physical solve for the board, skater and original animation targets.
//! The caller publishes forces and drive targets before entering this phase.
mod assembly_contacts;
mod diagnostics;
use super::{GamePhysics, SkaterRuntime, colliders, skeleton_colliders};
use skate_core::physics::{
    board::BodyId,
    board_step::{ATTACHED_REACTION_BASE, AttachedStep, BoardCollision, CollisionBody},
    skeleton_animation_record::{AnimationPartTransform, IDENTITY},
    skeleton_body::PART_COUNT,
};

pub(super) fn advance(
    physics: &mut GamePhysics,
    skater: &mut SkaterRuntime,
    truck_targets: [f32; 2],
) -> Result<(), String> {
    let before = diagnostics::snapshot(physics, skater);
    diagnostics::validate(&before, "before shared solve").map_err(|error| format!(
        "{error}; com_frame={:?}; lifted_com_frame={:?}; animation_root={:?}; biped_position={:?}; biped_surface={:?}",
        skater.animated_skeleton.board_frames.com_frame,
        skater.animated_skeleton.board_frames.lifted_com_frame,
        skater.animated_skeleton.roots.animation_to_world,
        skater.biped_ground.controller.state.position_368,
        skater.biped_ground.controller.state.surface,
    ))?;
    let mut board_volumes = colliders::world_volumes(&physics.board, &physics.settings);
    board_volumes.retain(|volume| skater.board_possession_live.volume_enabled(volume.body));
    let skeleton_volumes =
        skeleton_colliders::enabled_volumes(&skater.skeleton, &skater.skeleton_collision)?;
    // Each native assembly has its own query record and retention buffer.
    // Skeleton82BE5094 passes false to82768728: its edge threshold is -1,
    // whereas the board requests .999. GroundPipeline supplies the remaining
    // shared values. Do not let the second query overwrite the first's rows.
    let grind_segments = grind_nearby_segments(physics, skater);
    let mut contacts = physics
        .world
        .query_primitives(&board_volumes, physics.query, physics.retention)
        .to_vec();
    let mut skeleton_query = physics.query;
    skeleton_query.edge_cos_bend_normal_threshold = -1.0;
    let mut skeleton_world_volumes = skeleton_volumes.clone();
    skeleton_colliders::retain_world_volumes(&mut skeleton_world_volumes, &skater.skeleton_collision);
    contacts.extend_from_slice(physics.world.query_primitives(
        &skeleton_world_volumes,
        skeleton_query,
        physics.retention,
    ));
    // A valid grind owns the rail: its tube still supports the board, but the
    // tube's end face must not act as a wall for the board nor let the rider's
    // body slam the tip (regional-force wipeout). Drop the rail's non-support
    // board contacts and every rider-rail contact around any nearby primitive,
    // so a tessellated rail's neighbouring sub-chords are covered too.
    if !grind_segments.is_empty() {
        keep_grind_support_only(&mut contacts, physics.ticks, &grind_segments);
    }
    assembly_contacts::append(
        &mut contacts,
        &board_volumes,
        &skeleton_volumes,
        physics.board.collision_group(),
        &skater.skeleton_collision,
    )?;
    // Remote actors use separate reaction indices after skeleton and targets.
    // Never apply the single-skater self-culling bitmap to another player.
    let before_remote = contacts.len();
    for a in board_volumes.iter().chain(&skeleton_volumes) {
        for b in &physics.network_proxies.volumes {
            let (ac, ar) = super::network::bounds(a.primitive);
            let (bc, br) = super::network::bounds(b.primitive);
            if ac.distance_squared(bc) <= (ar + br + 0.05).powi(2) {
                assembly_contacts::append_pair(&mut contacts, a, b);
            }
        }
    }
    physics.network_contacts = contacts.len() - before_remote;
    physics.contact_count = contacts.len();
    let dt = physics.settings.step.simulation.time_step;
    let mut joints =
        skater
            .skeleton_joints
            .build(skater.skeleton.bodies(), ATTACHED_REACTION_BASE, dt);
    let mut drives = skater.skeleton_drives.build(
        skater.skeleton.bodies(),
        ATTACHED_REACTION_BASE,
        ATTACHED_REACTION_BASE + PART_COUNT,
        dt,
    );
    let skeleton_drive_count = drives.rows.len();
    //82D74FD8: persistent hand drives share the deck and skeleton reactions.
    // They belong to the off-board board controller; once it is off (mounted),
    // a stale selected hand must not keep pulling the arm toward the board.
    let board_controller_on = skater.skateboard_controller.fields.system_on_452;
    if board_controller_on {
        skater.board_possession.append_drives(
            physics.board.bodies()[BodyId::Deck.index()],
            [skater.skeleton.bodies()[3], skater.skeleton.bodies()[7]],
            BodyId::Deck.index(),
            [ATTACHED_REACTION_BASE + 3, ATTACHED_REACTION_BASE + 7],
            dt,
            &mut drives.rows,
        );
    }
    let bodies = skater
        .skeleton
        .bodies_mut()
        .iter_mut()
        .chain(skater.skeleton_drives.targets.bodies.iter_mut())
        .chain(physics.network_proxies.bodies.iter_mut())
        .collect();
    physics.board.advance_attached(
        &contacts,
        truck_targets,
        physics.settings.step,
        AttachedStep {
            bodies,
            contacts: &mut [],
            joints: &mut joints,
            drives: &mut drives.rows,
        },
    );
    if let Err(error) = diagnostics::validate(
        &diagnostics::snapshot(physics, skater), "after shared solve",
    ) {
        return Err(format!("{error}; input_bodies={before:?}; contacts={contacts:?}; joints={joints:?}; drives={:?}", drives.rows));
    }
    skater
        .skeleton
        .publish_physical_record(deck_frame(&physics.board));
    // These solved rows are consumed by the actual collision/drive feedback
    // phase; keep their identity and impulses after the shared solve.
    // Possession drives have no skeleton spy identity. They participate in the
    // same solve above, but must not enter the skeleton-only feedback batch.
    drives.rows.truncate(skeleton_drive_count);
    skater.solved_drives = Some(drives);
    Ok(())
}

/// True when the board is grinding, acquiring, or still holding a latched grind
/// target, so the grind system (not the raw collision) owns its rail.
fn grind_context_active(skater: &SkaterRuntime) -> bool {
    let grind = skater.player_input.processed.grind;
    if skater.grind.active_family().is_some() || grind.valid_1488 {
        return true;
    }
    if grind.second_start_1312 != [0; 4] || grind.second_end_1328 != [0; 4] {
        return true;
    }
    skater
        .trajectory
        .selector
        .selection()
        .is_some_and(|selection| selection.grind.is_some())
}

/// Every grind primitive near the board, as `[start, end]` float lanes. Using
/// all of them (not just the latched target) covers a tessellated rail's
/// neighbouring sub-chords, where the leading wheel or rider body actually
/// contacts the tip.
fn grind_nearby_segments(physics: &GamePhysics, skater: &SkaterRuntime) -> Vec<[[f32; 4]; 2]> {
    if !grind_context_active(skater) {
        return Vec::new();
    }
    let deck = physics.board.part_transforms()[BodyId::Deck.index()].translation;
    let span = 1.0f32;
    let min = [deck.x - span, deck.y - span, deck.z - span];
    let max = [deck.x + span, deck.y + span, deck.z + span];
    let Ok(indices) = physics.grind_world.query(min, max) else {
        return Vec::new();
    };
    indices
        .iter()
        .filter_map(|&index| physics.grind_world.primitives().get(index))
        .map(|primitive| [primitive.start, primitive.end])
        .collect()
}

/// While a grind family owns the board, the grind system owns the board's
/// relationship with the grinded rail. Keep only the rail's upward support
/// contacts (the surface the board rides on); drop the tube's cap, curl, side
/// and underside contacts, which the solver otherwise treats as walls and uses
/// to brake the board or drag it back near the ends. Contacts with any other
/// geometry, and contacts away from every nearby primitive, are untouched.
fn keep_grind_support_only(
    contacts: &mut Vec<BoardCollision>,
    tick: u64,
    segments: &[[[f32; 4]; 2]],
) {
    const RAIL_RADIUS: f32 = 0.35;
    // Contacts beyond either endpoint still belong to the rail's tip/post. A
    // boardslide crosses the rail, so its leading wheel lands lateral and past
    // the end; extend the neighbourhood so those are caught too.
    const END_EXTENSION: f32 = 0.5;
    // The contact must face along the rail's actual top direction (perpendicular
    // to the axis, up). Comparing against world up keeps the tube's rounded
    // tip/curl and side faces, which brake the board and drag it backwards.
    const SUPPORT_ALIGNMENT: f32 = 0.9;
    let mut frames: Vec<([f32; 3], [f32; 3], f32, [f32; 3])> = Vec::with_capacity(segments.len());
    for [s, e] in segments {
        let start = [s[0], s[1], s[2]];
        let end = [e[0], e[1], e[2]];
        let axis = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
        let length = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        if !(length > 1.0e-4) {
            continue;
        }
        let unit = [axis[0] / length, axis[1] / length, axis[2] / length];
        let up = [0.0f32, 1.0, 0.0];
        let along = up[0] * unit[0] + up[1] * unit[1] + up[2] * unit[2];
        let mut upmost = [up[0] - unit[0] * along, up[1] - unit[1] * along, up[2] - unit[2] * along];
        let upmost_length =
            (upmost[0] * upmost[0] + upmost[1] * upmost[1] + upmost[2] * upmost[2]).sqrt();
        if upmost_length > 1.0e-4 {
            upmost = [
                upmost[0] / upmost_length,
                upmost[1] / upmost_length,
                upmost[2] / upmost_length,
            ];
        } else {
            upmost = up;
        }
        if upmost[1] < 0.0 {
            upmost = upmost.map(|v| -v);
        }
        frames.push((start, unit, length, upmost));
    }
    if frames.is_empty() {
        return;
    }
    let trace = super::grind_tip_trace::enabled();
    contacts.retain(|contact| {
        let board_world = matches!(
            (contact.body_a, contact.body_b),
            (CollisionBody::Board(_), CollisionBody::StaticWorld)
        );
        let rider_world = matches!(
            (contact.body_a, contact.body_b),
            (CollisionBody::Attached(_), CollisionBody::StaticWorld)
                | (CollisionBody::StaticWorld, CollisionBody::Attached(_))
        );
        if !board_world && !rider_world {
            return true;
        }
        let point = contact.contact.position_on_b;
        let point = if matches!(contact.body_b, CollisionBody::Attached(_)) {
            contact.contact.position_on_a
        } else {
            point
        };
        let point = [point.x, point.y, point.z];
        let mut best: Option<(f32, [f32; 3])> = None;
        for (start, unit, length, upmost) in &frames {
            let rel = [point[0] - start[0], point[1] - start[1], point[2] - start[2]];
            let t = (rel[0] * unit[0] + rel[1] * unit[1] + rel[2] * unit[2])
                .clamp(-END_EXTENSION, length + END_EXTENSION);
            let closest = [
                start[0] + unit[0] * t,
                start[1] + unit[1] * t,
                start[2] + unit[2] * t,
            ];
            let distance = ((point[0] - closest[0]).powi(2)
                + (point[1] - closest[1]).powi(2)
                + (point[2] - closest[2]).powi(2))
            .sqrt();
            if best.is_none_or(|(best_distance, _)| distance < best_distance) {
                best = Some((distance, *upmost));
            }
        }
        let (distance, upmost) = best.expect("frames is not empty");
        if distance >= RAIL_RADIUS {
            return true;
        }
        // The rider never touches the rail it is grinding: drop every
        // rider-rail contact so its body cannot slam the tip.
        let normal = contact.contact.normal;
        let alignment = normal.x * upmost[0] + normal.y * upmost[1] + normal.z * upmost[2];
        let keep = if rider_world { false } else { alignment >= SUPPORT_ALIGNMENT };
        if trace && !keep {
            eprintln!(
                "GRIND_STRIP tick={tick} body={:?} rider={rider_world} dist={distance:.3} align={alignment:.3} n=({:.3},{:.3},{:.3}) p=({:.3},{:.3},{:.3})",
                contact.body_a, normal.x, normal.y, normal.z, point[0], point[1], point[2]
            );
        }
        keep
    });
}

pub(super) fn deck_frame(
    board: &skate_core::physics::board_runtime::BoardRuntime,
) -> AnimationPartTransform {
    let deck = board.part_transforms()[BodyId::Deck.index()];
    let mut frame = IDENTITY;
    for (axis, column) in deck.basis.columns.iter().enumerate() {
        frame[axis][..3].copy_from_slice(column);
    }
    frame[3] = [
        deck.translation.x,
        deck.translation.y,
        deck.translation.z,
        0.0,
    ];
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use skate_core::{
        math::Vector3,
        physics::contact::RetailContactInput,
    };

    fn rail(start: [f32; 3], end: [f32; 3]) -> [[f32; 4]; 2] {
        [
            [start[0], start[1], start[2], 0.],
            [end[0], end[1], end[2], 0.],
        ]
    }

    fn contact(body_a: CollisionBody, normal: [f32; 3], world: [f32; 3]) -> BoardCollision {
        BoardCollision {
            body_a,
            body_b: CollisionBody::StaticWorld,
            contact: RetailContactInput {
                position_on_a: Vector3::new(world[0], world[1] + 0.1, world[2]),
                position_on_b: Vector3::new(world[0], world[1], world[2]),
                normal: Vector3::new(normal[0], normal[1], normal[2]),
                restitution: 0.,
                static_friction: 0.,
                dynamic_friction: 0.,
                tag: 0,
            },
        }
    }

    #[test]
    fn grind_keeps_upward_support_and_drops_rail_side_and_cap() {
        let segments = [rail([0., 0., 0.], [0., 0., 10.])];
        let mut contacts = vec![
            // Head-on end face at the rail tip (normal along rail): removed.
            contact(CollisionBody::Board(BodyId::Deck), [0., 0., 1.], [0., 0., 9.9]),
            // Tube top supporting the board (near-vertical): kept.
            contact(CollisionBody::Board(BodyId::RightFrontWheel), [0., 1., 0.], [0., 0., 5.]),
            // Rider part against the rail end: removed (no rider-rail contact).
            contact(CollisionBody::Attached(0), [0., 0., 1.], [0., 0., 9.9]),
            // Tube side (horizontal normal) near the rail: removed.
            contact(CollisionBody::Board(BodyId::FrontTruck), [1., 0., 0.], [0., 0., 5.]),
            // Angled end/curl face +Y 0.7 near the rail: removed.
            contact(CollisionBody::Board(BodyId::FrontTruck), [0.7, 0.7, 0.14], [0., 0., 5.]),
            // Near-top support aligned with the rail upmost normal: kept.
            contact(CollisionBody::Board(BodyId::BackTruck), [0.2, 0.975, 0.1], [0., 0., 6.]),
            // Rider part far from the rail: kept.
            contact(CollisionBody::Attached(1), [1., 0., 0.], [0., 0., 40.]),
            // Board face far from this primitive: kept even though horizontal.
            contact(CollisionBody::Board(BodyId::Deck), [1., 0., 0.], [0., 0., 40.]),
        ];
        keep_grind_support_only(&mut contacts, 0, &segments);
        assert_eq!(contacts.len(), 4);
        assert!(matches!(contacts[0].body_a, CollisionBody::Board(BodyId::RightFrontWheel)));
        assert!(matches!(contacts[1].body_a, CollisionBody::Board(BodyId::BackTruck)));
        assert!(matches!(contacts[2].body_a, CollisionBody::Attached(1)));
        assert!(matches!(contacts[3].body_a, CollisionBody::Board(BodyId::Deck)));
    }
}
