# Object Dropper (host MVP)

The LB hold menu's third row, **Object Dropper**, is now a working host feature
when the current map has a prepared prop catalog. This is a deliberate host
reimplementation. It is **not** a recovery of the native Object Dropper: the
catalog, placement rules and contact geometry are not claimed to match retail,
and no native addresses or behaviour are asserted here.

## Controls

- Hold **LB + B** to open or close the dropper (on or off the board).
- **D-pad Up/Down** select a catalog entry.
- **`[` / `]`** (keyboard) or **D-pad Left/Right** rotate the ghost.
- **A** places the selected prop at the ghost; pressing A again duplicates it.
- **X** deletes the most recently placed prop.
- **B** closes the menu.
- Keyboard development fallback: **F7** toggles, arrows select, **Enter** places,
  **Backspace** deletes.

The menu keeps the simulation running and can be opened on or off the board.
It is unavailable during replay, a map transition or the pause menu. At most 32
props are kept; placing beyond the cap removes the oldest.

## Placement and collision

Props come from authored DMO templates already extracted for the map. Their
render batches are built by the same map grouping and material code as the
world (`skate_world::object_assets`), so retail-family materials use the retail
world shader and the rest use the world's PBR fallback (diffuse, normal, ORM,
emissive and lightmap exposure) rather than a hand-built material. The native
`dynamicobject.default` shader has no family mapping and therefore uses the PBR
fallback, matching the authored `native-props` supplement.

Placed props spawn a render entity and append their **render-mesh triangles** to the
live `BoardWorld` as static ground geometry, so the skater can land on them. The
packed surface code is taken from the template material's authored
`audio | physics<<7 | pattern<<12` value, matching the portable map path.

Because render geometry is used, contact classification and grindability are
approximations. Placed props are **static**: they are not movable DMO bodies,
they are not added to the grind-spline provider, and they do not carry the
native dynamicobject shader. Deletion removes the appended canonical triangle
range from `BoardWorld` (`BoardWorld::remove_triangles`); the map's own geometry
is never touched. A map change clears every placed prop.

## Asset requirement

The runtime loads
`ASSET_ROOT/private/dropper-templates/<MapName>/catalog.json` plus the
`<template_id>.skate` files it references. These are produced during setup by
`tools/asset_pipeline/dynamic_props.py::export_templates`, invoked from
`convert_map`. Each template is baked from the same authored DMO catalog as the
`native-props` supplement, in model-local space with its base centred at the
origin, and is written with the existing render-only `.skate` writer.

If the folder is missing, the feature stays disabled and the HUD row keeps its
0.3 baseline alpha. Re-run setup for each map after updating to generate the
catalogs. `dropper-templates/<MapName>-availability.json` records a failed or
skipped export.

## Extractor behaviour

`tools/extract_session_marker.py` still emits the dropper row at 0.3 alpha as
the disabled baseline and records `role: "dropper"` on those meshes. The runtime
parses `role` and restores the row to the authored alpha when a catalog is
available. No `hud.json` regeneration is required; existing installed overlays
are used as-is.

## Verification

- `tools.asset_pipeline.test_dynamic_props` covers classification, the
  base-centred export and catalog validation.
- `crates/skate-game/src/object_dropper.rs` tests cover surface packing, the
  placement transform and catalog parsing.
- `crates/skate-core` `BoardWorld::append_triangles` / `remove_triangles` tests
  cover insertion/removal order and query-mesh bookkeeping.

In-game placement, collision feel, ghost visuals and deletion across a real
map transition still require a running game and a generated catalog; none was
launched for this change.
