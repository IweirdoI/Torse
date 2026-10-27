# Cooked tables

`cook` converts `CompiledContent`, PNG identities and parsed locale maps into
validated runtime rows. The base-game producer runs from the repository root:

```sh
cargo run -p torse-tools --locked -- cook-base games/base/assets .work/base.tcook
```

`games/base/assets/cook.toml` assigns stable image GUIDs and lists source paths
and locale tags. The command uses the same fixture, block-action and inventory
registrations as the current base client. It validates PNG decoding and locale
syntax before replacing the output through a temporary file. Invalid input
leaves an existing output intact. Source paths must stay inside the asset pack;
each author file is limited to 8 MiB and decoded images to 64 MiB of RGBA pixels.
This entry point cooks the existing base composition; it is not a generic TOML
block-definition loader.

## Runtime contract

States are expanded in block-key order, with the last finite field varying
fastest. Each row stores its block key, local state index and field values,
occlusion, collision, six material slots and six optional overlay slots.
The dense material table is sorted by `(ContentKey, AlphaMode)`, so a texture
used with different alpha modes has separate slots. Images are sorted by GUID;
locale languages and message keys are sorted lexicographically. Input list
order does not change the output bytes.

PNG bytes remain external. An image row carries its GUID, source SHA-256,
content-key alias and relative PNG path. Replacing a PNG preserves its GUID
but changes presentation compatibility. The table does not contain decoded
pixels, mesh data or visual model geometry. Loaders still need the asset
catalog and PNG files. The current client continues to embed source assets;
switching its default input to cooked tables is follow-up work in issue #8.

The state rows are an occlusion/collision/material projection, not a complete
replacement for `CompiledContent` or `BlockStateRegistry`. For example, action
definitions and emission remain in the compiler/registry. Both full compiler
digests are retained and bound into the corresponding cooked digest, so those
behavior changes still affect compatibility.

## Format version 2

Integers and IEEE-754 `f32` values use little endian. A string is a `u16` UTF-8
byte length followed by exactly that many bytes. Overlong strings are errors;
they are never truncated. Counts below are `u32` unless stated otherwise.

The header is `TCOOK002`, version `2`, then four 32-byte SHA-256 values in this
order: cooked authority, cooked presentation, compiler authority, compiler
presentation. The four counted tables follow in this order:

1. States: block string, `u16` local index, counted `u16` field values, `u8`
   occlusion, collision shape, six `u32` slots, then six optional slots (tag
   `0`, or tag `1` and a `u32`).
2. Materials: content-key string and alpha `u8` (`0` opaque, `1` mask, `2` blend).
3. Images: `u128` GUID, 32-byte source SHA-256, key string, path string.
4. Locales: language string, counted pairs of message-key and value strings.

Collision tags are `0` none, `1` full cube, `2` lower slab, `3` panel,
`4` cross, `5` corner, `6` stair, `7` box, `8` compound. A box is six `f32`
values (min xyz, max xyz). A compound is a `u16` count followed by boxes.
Visual models are not valid collision shapes. Bounds must be finite and
ordered; standalone boxes stay inside the unit cell, while compound boxes
may extend through `[-1, 2]`, as required by `Shape::validate`.

Authority hashes use domain `torse-cooked-authority-v2`, the compiler authority
digest, and state identity/value/occlusion/collision rows. Presentation hashes
use domain `torse-cooked-presentation-v2`, the compiler presentation digest,
state identity/material/overlay rows, materials, images and locales. Hashes
frame variable strings with `u64` byte lengths and variable lists with `u64`
counts; scalar/shape encodings follow the wire format. These digests detect
corruption and compatibility changes; they are not signatures from a trusted
publisher.

This replaces the unpublished branch format `TCOOK001`/version 1. Readers
explicitly reject it; regenerate cooked files from source. World saves and
network formats are unaffected.

## Reader and writer validation

`write_cooked` returns `Result<Vec<u8>, CookError>` and validates the same
invariants as `read_cooked`. Readers reject truncated/trailing bytes, stale
digests, duplicate or unsorted rows, invalid aliases/geometry/masks and
out-of-range material references. State indices start at zero and are
consecutive per block; value tuples have consistent arity and increase.

Default limits are 64 MiB encoded input, 64 MiB accounted owned storage and
1,000,000 aggregate items. Count checks precede capacity reservations and
also check the remaining wire bytes. `read_cooked_with_limits` lets a caller
lower those ceilings. Owned storage accounts for row/value/string storage
and estimated map metadata; it is not an exact process-memory ceiling.
The existing content ceilings also apply: at most 65,535 state/material rows,
4,096 states per block and 64 boxes per compound.

Regression tests cover deterministic base-file generation, registry row
equivalence, successful replacement and failed-cook preservation, digest
separation and framing, every truncated prefix and single-byte bit flip,
huge counts, resource limits, invalid geometry/aliases/slots and duplicate
serialized locale keys.
