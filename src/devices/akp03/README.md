# AKP03 offline codec

This self-contained module prepares and decodes bytes only. Include `mod.rs`
from a Rust 2024 crate with `serde` 1 (the `derive` feature) and
`jpeg-encoder` 0.6. Its tests additionally use `serde_json` 1,
`jpeg-decoder` 0.3, and `sha2` 0.10.

* `profile`: exact profile IDs, dimensions, HID identities, and revision-2
  report-descriptor comparison. These do not discover or select a device.
* `input`: complete-report and strict ASCII-hex decoding with typed errors and
  tagged input JSON. Original push observations never imply a release.
* `controls`: profile recommendations and normalization to typed physical
  events. The caller supplies the connection generation and shared clock
  sample. Except for the specified isolated Stop control, unspecified
  proximity is conservatively treated as near selection.
* `output`: zero-filled 1,025-byte command reports and bounded JPEG framing.
  Raw JPEG framing checks length and boundary markers only.
* `image`: exact-size RGB preparation, revision-2 clockwise rotation, quality
  90 JPEG encoding, RGB-to-report composition, and upright numbered test tiles.
* `layout`: row-major logical diagnostic positions, scoped historical revision-2
  control locations/directions, and validated preview geometry.
* `probe`: an injected-clock HID state machine for the revision-2 diagnostic,
  exact raw-descriptor gating, bounded capture, and synthetic transport tests.

Recommendations are not active bindings. An ACK, a decoded event, generated
reports, or matching identity/descriptor does not establish native evidence,
display visibility, command readiness, or authorization. The shared input,
evidence, and gesture layer must independently validate any eventual use.

CLI/worker wiring, supervised process containment, SVG previews, semantic
presentation, and shared artwork are intentionally not implemented here. Native
AKP03 probing fails closed when the OS does not expose the exact interface number
or original report-descriptor bytes; parsed HID caps are not a substitute.
