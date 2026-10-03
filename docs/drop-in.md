# Off-board drop-in

Status: working and user-verified on the default/test map and retail maps
(University). This document records the native behavior, the ported pieces and
the diagnostics left in the tree.

A drop-in is performed on foot: walk to the top coping/deck of a quarter pipe,
halfpipe or ramp and press Y (the remount toggle). The stock MotionGraph
`Mount.Ground.IntoDropIn` branch takes over, plays the `DROPIN_*` clips, mounts
the board and rides the transition into
`OnBoard.Grinding.DroppingIn`.

## Stock graph source

- `state/MotionGraphIncludes/mount.xml`
  - `IntoDropIn` (line 93): precondition `(LocoState Stand OR CurrentState
    OffBoard.OBGround.IntoStand) AND DistToEdge <= 1.5 AND IsHoldingSkateboard`;
    creates `OB_DropIn`/`OB_Traj` and plays `DROPIN_BIGSTEP_INTO` /
    `DROPIN_STEP_INTO` / `DROPIN_STAND_INTO`.
  - `Cycle`/`TryToLeave` (141) creates `BipedBoardOnGround`, calls `EndGesture`
    and waits on `IsArmChannelPlaying`.
  - `OutOfDropIn` (189) creates `TransitioningOnOffBoard`, `OB_Mounting` and
    `OB_DropIn`, plays `DROPIN_STAND_OUT`, then targets
    `OnBoard.Grinding.DroppingIn.Nose`.
  - Sibling `IntoMount` (213) is the ordinary flat-ground remount.
- `state/MotionGraphIncludes/Grinds/Grinds.xml` `DroppingIn` (line 6) is gated on
  `IsDroppingIn` and plays `DROPIN_RIDE_CYC` / `DROPIN_RIDE_FAKIE_CYC`.

## Ported changes

### Ledge detection feeds `DistToEdge`

`DistToEdge` reads `OffBoard+116`, which is only real when the off-board ground
query classifies `kind == 2`:

- `crates/skate-core/src/player/offboard/ground_query/consume.rs`
  `interpret_hits`: the single-sided ledge branch (`h0` or `h1` with a short far
  line) is now evaluated before the centre-line fallback. Previously
  `(h0 && h1) || h4` forced `kind == 3` whenever the centre line grazed the lip,
  so `state_752` and therefore `OffBoard+116` were unreachable at every authored
  seam. Regression:
  `single_sided_ledge_survives_a_centre_line_hit` in the same module's tests.

### Retail maps have no off-board edge candidates

Imported maps build `QueryMetadata.static_edges = []`, so the biped ground query
never sees a coping. The loaded grind spline segments are the same authored
segment asset native's edge collection reads, so they are now supplied as
indexed edge bodies:

- `crates/skate-game/src/physics/offboard/ground_sync.rs`
  `SceneService::edge_candidates`: queries
  `StaticProvider::query(search.min, search.max)` (existing octree) and builds
  `EdgeBody`/`IndexedEdgeBody` world-space segments. The indexed path is used
  because `PrimaryEdges::Normal` rejects any body whose origin is more than 15 m
  from the query.
- `crates/skate-game/src/physics/biped_ground.rs` `submit_geometry` passes
  `physics.grind_world` into `ground_geometry::State::submit`.

Note: `crates/skate-game/src/physics/offboard/ground_state.rs` is dead code (not
declared in `offboard/mod.rs`); the live path is `SceneService`.

Host-placed ramps from the Object Dropper contribute coping/ledge edges through
the same `SceneService`: `GamePhysics::host_edges` holds world-space segments
built from the prop's top rim/crease (`object_dropper::build_drop_edges`) and is
bounded in `edge_candidates` like the authored splines. See
[object-dropper.md](object-dropper.md).

### Mount drop-in conditions

- `crates/skate-game/src/graph_host/motion_conditions.rs`
  - `IsArmChannelPlaying`: true while a `CharacterGesture` arm channel
    (`GestureBoth`/`GestureRight`/`GestureLeft`) is live. `Mount.TryToLeave` ends
    the gestures and waits on this.
  - `IsDropInWithoutTransition`: resolved-false leaf. The native producer is not
    recovered, so the canonical `Grinding.DroppingIn` transition is kept instead
    of aborting on an unsupported node.
  - Unsupported graph conditions are evaluated as false and recorded in
    `MotionHost::errors`; `skater_animation` still fails a tick when the error
    list is non-empty, which is why the two leaves above were needed.
- `crates/skate-game/src/physics/animation_grind.rs` +
  `crates/skate-game/src/physics/animation_phase.rs`: the `OB_DropIn` flag
  (`Processed2488` bit 28) feeds `dropping_in_324`, so `IsDroppingIn` is true
  during the off-board drop-in and `Grinding.DroppingIn` can be entered without a
  physical grind contact.

### Non-finite padding lane

Off-board animation/velocity sources leave `NaN` in the unused 4th lane of some
`[f32; 4]` vectors. The trajectory math and the solver checks only read xyz, so
the checks that rejected the whole vector were narrowed:

- `crates/skate-core/src/player/offboard/air_selector/candidates.rs` `prepare`
- `crates/skate-core/src/player/offboard/air_selector.rs` `validate_request`
- `crates/skate-game/src/physics/biped_ground/sync.rs`: the rebuilt launch
  position sets `w = 1.0` (affine point) instead of publishing the frame's NaN
  padding.

### Drop-in owns the physical transition

`crates/skate-game/src/physics/biped_ground/sync.rs` `Owner::prepare_air`
suppresses the off-board biped air launch while `OB_DropIn` is set, so the
drop-in mounts the board instead of treating the step to the lip as walking off
a ledge and free-falling.

### Arm overlay teardown

An arm overlay could outlive its `CharacterGesture` owner and keep posing the
arm (the arm that held the board). `crates/skate-game/src/graph_host/motion.rs`
adds `behavior_is_character_gesture` / `trim_gesture_channels`, called from
`crates/skate-game/src/skater_animation.rs` after the motion controller update:
when no `CharacterGesture` owner is active, the arm channels are ended.

### Off-board hand drive vs. mount/teleport

The off-board possession hand drives share the deck and skeleton reactions and
are built every solve tick:

- `crates/skate-game/src/physics/solve.rs`: the drives are only appended while
  `SkateboardControllerFields::system_on_452` is set. After mounting the stale
  selected hand no longer pulls the arm toward the board (previously seen as
  `hand=1 hand_active=true system_on=false`).

### Re-seat the board on off-board entry

A stale held state can survive a mount (`system_on=false`, `state_448==1`).
`crates/skate-core/src/player/lifecycle.rs`
`start_skateboard_controller` now always calls `hold_skateboard()` on off-board
entry, even when `state_448==1`, refreshing the hand drive frames. Without it a
dismount after a drop-in reused stale frames and the held board oscillated on an
"invisible spring" until the player released and re-grabbed the board.

## Diagnostics

All are opt-in and disabled by default.

- `SKATE3_DROPIN_TRACE=1` enables `crates/skate-game/src/physics/drop_in_trace.rs`
  (`DROPIN_TRACE` lines: physical state, `flags2480`/`flags2488`, MotionGraph
  state path, `DistToEdge` inputs, grind family/entry/`drop324`, deck `spin`,
  live `channels`, hand/`hand_active`/`system_on`). Optional window:
  `SKATE3_DROPIN_TICKS=start:end`. It also dumps the MotionGraph capability
  report as `MOTION_GRAPH_CAPABILITY`.
- `SKATE3_DROPIN_EDGE_PROBE=1` enables the bounded `DROPIN_PROBE` candidate
  selection probe in
  `crates/skate-game/src/physics/offboard/ground_geometry.rs` and the
  `DROPIN_EDGE` classification line.

These are development aids, not gameplay behavior; they change no state.

## Known remaining limitations

- The physical drop-in is approximated by the off-board launch/adhesion path; a
  full native transition placement (board snapped onto the transition and the
  specialized tipslide drop-in exit) is still not ported. The ride reaches
  `Grinding.DroppingIn` and is stable in testing, but residual spin during the
  landing depends on the incoming trajectory.
- `IsDropInWithoutTransition` remains a resolved-false leaf until its native
  producer is recovered.
