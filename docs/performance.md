# Large map performance

The September 6, 2026 performance pass preserves the full render geometry and
collision data. It does not add LODs or simplify physics.

Changes:

- A bounds hierarchy selects collision clusters instead of scanning all clusters.
  Results retain their original order, including equal-distance hits.
- Conservative triangle bounds reject impossible line and primitive contacts
  before the expensive narrowphase. Predictive separation and fatness are included.
- Byte-identical textures and equivalent rendered materials share resources and
  batches. Texture color space and all currently rendered material fields remain
  part of their identity. Every source render triangle is retained.
- Render meshes and textures release their CPU copies after GPU upload. The
  decoded map package is released after physics and rendering have consumed it.
- The normal development build now optimizes project code at level 3, matching
  the existing dependency optimization level.

## Repeatable measurement

From the project directory in PowerShell:

```powershell
./Build.ps1
$env:SKATE_PERF_REPORT = 'logs/performance.json'
./bin/skate-game.exe --assets assets --map maps/Skate_2_New_San_Vanelona.skate
Remove-Item Env:SKATE_PERF_REPORT
```

The benchmark runs continuously regardless of window focus, waits 10 seconds
for warmup, records roughly 15 seconds, writes JSON, and exits. Leave the player
stationary for spawn comparisons. The environment variable enables instrumentation;
normal launches do not collect reports or change their window-focus behavior.
Frame intervals include renderer synchronization. Main schedule and render timings
overlap and must not be added together. Detailed physics section timings include
warmup and nested scopes; these must not be summed either.

Local San Vanelona measurements at 1280 x 800, stationary spawn, RTX 5090:

| Metric | Before | After |
| --- | ---: | ---: |
| Average FPS | 43.6 | 50.7 |
| Main schedule per frame | 10.56 ms | 2.39 ms |
| Physics per tick | 6.44 ms | 0.74 ms |
| Frame interval p95 | 128.1 ms | 94.5 ms |

Sources are local ignored reports `logs/perf-baseline-continuous.json` and
`logs/perf-render-detail.json`. Repeated optimized runs averaged 49–52 FPS.
Process working set fell from about 10 GB to roughly 1–2.3 GB after loading.
These are spawn measurements, not a reproduction of the reported 10 FPS camera
position. Render preparation remains the largest measured cost, and frame spikes
remain. Low total CPU/GPU utilization does not exclude a CPU render bottleneck.

Spatial render splitting was also tested, but increased entity/batch overhead and
reduced FPS on this map, so it is not included.

Validation includes the full skate-core suite and game suite (43 passed, 21 existing
ignored tests), differential collision queries against the unfiltered implementation
across 420 varied poses, material/texture identity checks, and exact render triangle
retention tests. Startup capture checks rendering/integration, not retail parity.

## Follow-up: Bevy lightmap binding cache

GPU diagnostics confirmed the RTX 5090 Vulkan adapter with binding arrays,
non-uniform indexing, and indirect drawing enabled. Local source inspection and
profiling found repeated lightmap bind-group creation in Bevy's
`prepare_mesh_bind_groups`. The [tracked Bevy patch](../vendor/README.md) caches
those bindings with resource-identity checks and bounded retention.

Same stationary-spawn benchmark, before and after this patch:

| Metric | Before cache | After cache |
| --- | ---: | ---: |
| Average FPS | 52.3 | 237.1 |
| Frame interval p95 | 94.5 ms | 5.15 ms |
| Render preparation per frame | 15.52 ms | 1.19 ms |
| Mesh binding interval per call, including warmup | 10.39 ms | 0.66 ms |

Reports: `logs/perf-bindings.json` and `logs/perf-cache-repeat.json`. The first
cache trial averaged 158 FPS, but overlapped test compilation and is excluded
from the clean comparison. The normal window presentation settings are unchanged.
GPU telemetry during the clean repeat sampled roughly 23–33% usage, compared
with the earlier single-digit readings. Full utilization is not the goal: this
GPU can render this view at high FPS without approaching full utilization.

An additional render-only camera sweep averaged 91.1 FPS, with a 24.9 ms p95
frame interval (`logs/perf-cache-sweep.json`). This rotates through different
views to exercise changing visibility and newly extracted lightmaps; it does not
move the skater or alter collision behavior. Enable it alongside the usual
benchmark variable with `$env:SKATE_PERF_CAMERA_SWEEP = '1'`, and remove it
afterwards with `Remove-Item Env:SKATE_PERF_CAMERA_SWEEP`. It only runs when
`SKATE_PERF_REPORT` also enables the benchmark plugin.

The sweep is a different workload, not an FPS comparison against the stationary
baseline. Exploration can still encounter heavier views and loading/shader spikes.
Validation: GPU cache regression test passed, game suite passed (43 passed,
21 existing ignored), and the staged executable completed a normal city startup
capture with benchmark settings disabled.

## City-facing dips at 1440p / 8x MSAA

The next pass uses the user's saved 2560 x 1440, 100% internal resolution,
8x MSAA, unlimited FPS settings and the same rotating-camera benchmark.
Do not compare these results directly with the earlier stationary 1280 x 800 run.

| Metric | Before | After |
| --- | ---: | ---: |
| Average FPS | 84.9 | 119.1 |
| Frame interval p95 | 28.9 ms | 23.5 ms |
| Render preparation per frame | 5.42 ms | 1.49 ms |

Reports: `logs/city-baseline.json` and `logs/city-double.json`. Temporary direct
instrumentation measured roughly 3–4 ms in mesh binding preparation before the
final fix and about 0.05 ms after warmup. GPU telemetry during the optimized run
sampled roughly 24–39% usage. Heavy views and frame spikes remain.
The final clean build, with temporary tracing and GPU telemetry polling removed,
averaged 145.0 FPS with an 18.9 ms p95 frame interval (`logs/city-final.json`).
The 119–145 FPS range illustrates run/instrumentation variation; it is not a
guaranteed minimum while exploring the whole map.

Bevy's `collect_buffers_for_phase` exchanges two buffer allocations every frame.
A single-table cache therefore misses every frame even with static resource
contents. The patch now retains both phase tables, checks resource identities
and lightmap revisions, and shares unchanged tables without visiting every slab.
The preceding one-table trial did not materially improve the sweep and is not
the implementation shipped here.

The game also reserves geometrically growing instance-buffer capacity before
Bevy writes it, bounded by device limits. This reduces reallocations when visible
counts increase; it does not change logical counts or dispatch extra instances.
Mesh morph-target lookup is refreshed on GPU asset changes, and identical PBR
materials can share handles while each mesh retains its own baked lightmap.

No triangles, collision detail, lighting, or draw distance were removed. GPU
occlusion culling is also relevant to a dense city, but the current engine's
version is experimental: [Bevy's migration notes](https://bevy.org/learn/migration-guides/0-18-to-0-19/#occlusion-culling-is-no-longer-experimental)
describe correctness fixes in 0.19. It was not enabled as an unverified workaround.

Validation: the explicit GPU regression test and game suite (46 passed,
21 existing ignored) passed. The staged build completed the normal San Vanelona
startup capture at the user's saved settings, with benchmark overrides disabled.

## Runtime occlusion culling

The Escape graphics menu now supports GPU occlusion culling, default On. Existing
settings files without that field pick up the default. `SKATE_OCCLUSION=0` or `1`
overrides the saved value at launch for A/B runs. The two-phase GPU algorithm
uses a depth prepass and dynamically rejects hidden meshes; no map baking, LOD
generation, distance cutoff, or collision changes are involved.

This backports [Bevy #22603](https://github.com/bevyengine/bevy/pull/22603) to
0.18.1's core pipeline: conservative depth-pyramid sampling at non-power-of-two
resolutions. It also fixes a reproduced GPU validation failure in 0.18.1's late
preprocessing pass by making its indirect argument buffer binding read-only.
Details and provenance are in `vendor/README.md`.

Same saved 1440p / 100% / 8x MSAA settings, rotating view, no concurrent builds:

| Trial | Average FPS | Frame interval p95 |
| --- | ---: | ---: |
| Original material batches, culling Off | 124.1 | 22.1 ms |
| Original material batches, culling On | 128.0 | 21.3 ms |
| 128-unit spatial batches, culling On | 72.4 | 51.2 ms |

Reports: `logs/cull-off.json`, `logs/cull-on2.json`, `logs/cull-cell128.json`.
The small On/Off difference is within plausible run variation, not evidence of
a major FPS win. CPU/render submission overhead remains significant even when
the GPU skips hidden geometry. These sweeps are not a minimum-FPS guarantee.
Spatial splitting increased batches from 30,097 to 55,850 and was removed from
the final implementation. All 8,214,063 render triangles remain available.

The setting remains available in the Escape menu, but `Off` is the recommended
value on this hardware: the A/B above shows no gain to pay for the extra depth
prepass. The saved `graphics.json` for the Vega 8 machine has it `Off`.

Validation: city sweep and normal startup capture completed without GPU errors
at 8x MSAA. The game suite has 47 passing tests and 21 pre-existing ignored tests,
including changing culling, MSAA and internal resolution together. The explicit
GPU lightmap cache regression test also passed. The startup
image was visually inspected. Manual menu testing was interrupted by the user
stopping Computer Use, so live UI toggling has not been visually verified.

## Draw-call cost: exhausted

The three structural levers for per-draw CPU cost have all been tried. Do not
re-attempt them without new evidence.

| Lever | Result |
| --- | --- |
| Material batch merging by identity | 8,546 source groups reduced to 4,868; already canonical, and `WorldMaterialKey` deliberately does not split `tangent_mode` groups so shading is unchanged |
| GPU occlusion culling (backported `#22603`) | 124.1 → 128.0 FPS at 1440p / 8x MSAA, inside run-to-run variation |
| Spatial splitting into 128-unit cells | 30,097 → 55,850 batches, slower; reverted |

**Why occlusion culling did not pay off.** The bottleneck is CPU per-mesh
overhead in the render thread, not draw count or triangle count. The two
settings that actually moved the frame differ in exactly one respect:

- `Draw distance` sets `Visibility::Hidden`, which removes the mesh from
  extraction entirely, so the CPU never queues it. It cuts CPU *and* GPU work.
  Measured 54.1 → 73.9 → 81.7 FPS for Full / 150 m / 75 m.
- `Occlusion culling` rejects the mesh on the GPU. The CPU still extracts,
  queues and writes the indirect instance for every mesh, so only GPU work
  drops.

This is why a lower batch count does not automatically mean a faster frame:
splitting raised the mesh count, and the per-mesh CPU cost is what dominates.

`graphics_menu::cull_distant_batches` itself was measured rather than assumed:
55 us per frame over 572 calls at 4,868 batches, i.e. 0.3% of a 17 ms frame and
well under thermal run-to-run noise. Gating it on camera movement was
considered and rejected — it would add a stateful early-out to save less than
the measurement error, and it would not help at all while actually skating.

Hierarchical LOD for distant geometry is the only remaining lever that reduces
the mesh count itself. It is deliberately not implemented: it is the one
change that would alter distant-geometry appearance, and it is not worth
trading fidelity for a low single-digit frame share on this hardware.

## Low-end graphics options

The game menu exposes optional, lossy settings for weak GPUs. All default to
the original full-quality behavior and are persisted with the other graphics
settings in `settings/graphics.json`.

| Menu row | Values | Effect |
| --- | --- | --- |
| Shadows | Default / Reduced / Off | `Reduced` halves the directional shadow-map size (2048→1024, point 1024→512); `Off` disables shadow casting on the map sun and both character shadow sources. |
| Texture detail | 100% / 50% | `50%` box-filters every non-cube texture to half dimensions before upload and adds a mip chain to albedo. Applied on the next map load. |
| Environment props | On / Off | Skips the render-only `native-props` package at map load. No collision is involved. |
| Backdrops | On / Off | Skips the render-only `native-backdrops` package at map load. |
| Draw distance | Full / 150 m / 75 m | Hides a static batch only when its whole axis-aligned box lies beyond the limit, so nothing inside the range pops. |

A/B overrides for reproducible measurements: `SKATE_SHADOWS=0|1|2`,
`SKATE_TEXTURES=50|100`, `SKATE_PROPS=0|1`, `SKATE_BACKDROPS=0|1`,
`SKATE_DISTANCE=full|medium|short`.

`Shadows` and `Draw distance` apply immediately. `Texture detail`,
`Environment props` and `Backdrops` apply when a map is loaded or reloaded.
Draw distance culls by nearest point on each batch's bounds; because batches
merge all geometry sharing a material, a batch that spans into the visible
range is always kept. This is a conservative approximation of a draw-distance
cut and does not require splitting the merged batches.

### Measured on an AMD Vega 8 (Ryzen 5 3500U), University, 1280x720, MSAA off, occlusion off, uncapped development build

| Configuration | Average FPS | Frame p95 |
| --- | ---: | ---: |
| Defaults (saved settings, VSync off) | 21–31 | 111 ms |
| Shadows Off | 41.5 | 39.5 ms |
| Shadows Off + draw distance 150 m (rotating camera) | 73.9 | 24.3 ms |
| Shadows Off + draw distance 75 m (rotating camera) | 81.7 | 20.7 ms |
| Shadows Off + 150 m + props/backdrops off (stationary) | 69.8 | 28.9 ms |

Shadow casting and per-batch draw culling were the dominant costs on this iGPU;
texture detail showed no measurable change at 720p. These are single-machine
observations, not guarantees: run-to-run variation is significant and the
unstaged development build still carries debug assertions and overflow checks.

### CPU-side cost reduction

Three findings from a Chrome trace of a 60 FPS frame, recorded on the Vega 8
machine described above. Percentages are medians of three alternating A/B pairs
because single runs on this machine vary by more than 10 FPS.

`customiser_parts::update` rebuilt the JSON character preview, the selection
strings and the per-colour `resolve` validation on every frame even with the
customiser closed, because its early return sat below that work. With the menu
closed `preview` is only `draft.clone()`, so comparing `applied` to `draft`
directly is equivalent and needs no clone. The modding snapshot was likewise
built three times per frame (PreUpdate, every physics tick, and Update) while
only live Lua scripts read it; it is now built only when a script is running or
a rescan could start one, so a newly loaded mod still receives real world state.
Together these cut **1.23 ms of CPU per frame**, consistent across every A/B
pair, which is **+1.2% FPS at 720p** and **+2.9% at 25% internal scale**. The
720p gain is small because that frame is GPU-bound; the saving is headroom.

`command_buffer_generation_tasks` averages 2.13 ms per frame, but the trace
places **all** of it on the render thread inside `run_graph`,
`submit_graph_commands` and `main_opaque_pass_3d`, with none on the main
schedule. It is Bevy's render-graph bookkeeping, not gameplay code: the game
adds a single custom node (`retail_exposure::ExposureNode`) and spawns a single
production `Camera3d`, so there is no redundant view multiplying the graph. The
only lever is draw-call count, which is why batch merging is the remaining
render-thread opportunity.

Release is now the default build profile. Both profiles compile at `opt-level =
3` and the test suite is identical (272 passed, 4 pre-existing failures), so the
simulation arithmetic is unchanged; the 15% is purely the absence of
debug-assertions and overflow-checks.

## Solve-phase allocations: measured, not worth it

`physics::solve::advance` copies the board contact rows out of
`BoardWorld::query_primitives` before running the skeleton query, because
`query_primitives` clears the world's own row storage on every call
(`board_world.rs:279`). The copy is load-bearing; removing it would drop the
board contacts.

The *destination* was not: replacing the per-tick `.to_vec()` with a
`GamePhysics`-owned buffer handed back at the end of the tick was implemented
and verified bit-exact by the determinism harness (960-line traces byte
identical on `Flat` and `Course`). It is documented here as a negative result
because it was reverted.

Allocation counts, from a temporary counting global allocator, normalised to
the ~900 physics ticks each run performs:

| Variant | Allocations per physics tick |
| --- | --- |
| baseline | 165,415 / 165,880 |
| reused buffer | 165,206 / 165,860 |

The change removes exactly one allocation per tick out of roughly 165,000 —
about 0.0006%, below the ~335/tick run-to-run spread. Paired frame-time A/B on
University confirmed it: per-pair physics-tick deltas were +1.18, +0.64, -0.05,
-0.03, +0.06 and +0.37 ms, i.e. sign changes with no directional signal.

It was reverted because it added a field to `GamePhysics` plus a hand-back that
two early-exit paths (`skeleton_colliders::enabled_volumes` and the
post-solve diagnostics check) skip, in exchange for an effect that cannot be
measured above noise. Note the surrounding context: the process performs on the
order of 165,000 allocations per physics tick in total, so the solve-phase copy
is not where allocation pressure lives.

The determinism harness in `crates/skate-game/src/tests/determinism.rs` is the
tool that made this checkable without relying on frame times. It runs 240 ticks
on `Flat` and `Course` and digests body rates, skeleton rates and poses,
`solved_contacts()` and `contact_reports()`, and it is what any future physics
change must pass before it is considered.

## Main-thread cost is Bevy framework overhead, not game logic

The main thread was measured at 8.11 ms per frame (median of 3 release runs,
University) with 7.95 ms of it reported as `main_schedule_ms_mean`, which spans
`First` through `Last`. Only 1.05 ms of that was the game's own work: bracketing
each of the four `FrameSet`s that `app.rs` chains in `Update` accounts for
`set_animation` 0.82, `set_verification` 0.12, `set_assets` 0.05 and
`set_physics` 0.04 ms, and bracketing the whole of `Update` measures 1.05 ms, so
almost nothing of the schedule sits outside those sets either.

The rest is Bevy framework work in the phases the game does not own:

| block | ms |
| --- | --- |
| game logic (`Update`, all four `FrameSet`s) | 1.05 |
| `bevy_transform` propagation (`PostUpdate`) | 1.44 |
| `bevy_ui` layout (`PostUpdate`) | 0.77 |
| `bevy_ui` focus (`PreUpdate`) | 0.68 |
| `First` after the frame timer | 0.01 |
| unaccounted in `PreUpdate`/`PostUpdate`/`Last` | ~4.17 |

The conclusion that matters for prioritisation: game logic is about 13% of
main-thread time, so shaving milliseconds out of our own systems cannot close a
4 ms gap. Transform propagation and UI together are 2.9 ms, which is where the
next real work is. The unaccounted remainder is spread across the phases the
game does not configure, and these brackets are wall-clock spans in a
multi-threaded executor, so the numbers do not sum to the schedule total.

This was measured by temporarily bracketing each `FrameSet` and each Bevy
system set with the existing `Scope` mechanism, then removing the brackets. They
are not left in the tree because Bevy 0.18 has no per-system timing available
without pulling in a crate that is not in the lockfile, so the brackets would
have to be maintained by hand and they add schedule nodes to every frame.

Two measurements that do not reconcile, recorded rather than quietly averaged:
this run reports `frame_ms_mean` 11.75 ms, while an earlier release run reported
20.62 ms on the same map. The earlier reports are no longer on disk and the map,
settings and build differ between the two, so the older figure could not be
reproduced or explained. The A/B comparisons in this document are internally
consistent because both sides carried identical instrumentation.
