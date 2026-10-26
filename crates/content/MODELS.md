# Static models and compound shapes

`Shape::Model(RenderModel)` stores at most 1024 precompiled quads. Each quad
contains outward positions, UVs, a unit normal, a material identity and alpha
mode, optional overlay and tint channel, a boundary-cull direction, shading
policy and optional back face. Vertices must be finite and stay in `[-1, 2]`;
UV components are finite and bounded by 16 in magnitude. Importers bake static
transforms before content assembly.

Models are visual-only. Collision and selection reject visual models and may
instead use `Shape::Compound`, a non-nested collection of at most 64 boxes.
Each authoritative box retains the existing unit-cell validation. An empty
compound explicitly contributes no surfaces. Render complexity does not create
implicit physics geometry or cross-cell collision indexing.

The model variant holds an `Arc<RenderModel>`, so cloned descriptors and registry
entries share immutable quad storage. Pointer addresses never enter identity.

Finite state render-shape overrides select complete models, including their
material and UV assignments. `BlockStateRegistry` exposes borrowed effective
shape queries so extraction need not clone a model or a compound per cell.

The CPU renderer accepts resolved `ModelQuad` values through `append_model`.
The caller resolves material identities and supplies its conservative boundary
culling policy. The operation checks its face budget before changing the mesh,
preserves UVs and layers, and emits reversed winding and normals for back faces.
The mesh carries one shading-policy value per vertex in `shades`.

## Identity and compatibility

Existing preset tags retain their byte encodings; compound and model tags are
7 and 8. Model appearance contributes to presentation identity. Authoritative
compound shapes contribute to authority identity independently.

Finite state overrides were previously missing validation and digest coverage.
They now validate canonical legal value combinations, geometry and conservative
coverage, and are sorted before hashing. A tagged extension is appended only
when a relevant state override exists. Packs without overrides preserve their
old streams. Packs with overrides receive corrected authority or presentation
identities and require the ordinary compatibility rejection or an explicit
offline migration before an existing archive can be reused.

World cell encodings, finite state identifier representation and network packet
layouts are unchanged. Callers must continue compiling the same content at
both ends of a session.

## Validation

Run `cargo fmt --all -- --check`, workspace Clippy and the full workspace tests.
Focused tests cover invalid model coordinates and winding, model and box
limits, physics/model separation, state profile identity and canonical ordering,
UV and overlay preservation, back faces, conservative culling and atomic mesh
budgets. Existing preset, movement, persistence and replication tests remain
part of the workspace regression suite.
