# Display devices

This appendix defines the CT and AKP03 functional compatibility boundary for
**copilot-controller**: offline decoding, display preparation, control naming,
profile recommendations and explicitly invoked diagnostics. Commands in these
families retain the neutral `ct-` and `akp03-` prefixes.

All byte offsets are zero-based. Hexadecimal byte sequences have no implicit
report identifier or terminator. An accepted write, matching identity or decoded
input is **not** permission to deliver an action.

## 1. Supported functionality and qualification

| Device | Identity and geometry | Implemented | Not established |
|---|---|---|---|
| CT | USB `2ec2:0003`; center 360×270, sides 60×270, wheel 240×240 | Serial decoder, raw-control/touch naming, profile adapter, RGB565, offline tile renderer, Windows raster/PNG backend, bounded Windows/macOS ARM64 diagnostic transport | Production live transport, physical orientation/rendering acceptance, general input qualification, reconnect, normal action dispatch |
| AKP03 original | HID `0300:1001`, usage `ffa0:1`; six 60×60 keys | Complete-report decoder, unrotated RGB/JPEG preparation, logical preview, profile adapter | Physical interoperability, explicit releases, live original-profile probe |
| AKP03 revision 2 | HID `0300:3002`, usage `ffa0:1`; six 64×64 keys | Explicit-state decoder, clockwise-rotated JPEG preparation, preview/profile, optional Windows/macOS HID discovery and bounded probe | Production controller qualification, Windows live acceptance, sustained use, reconnect, rollover |

### Capability reporting

* CT protocol decoding and pixel conversion are available. Its normal
  `live_transport` and `hardware_qualified` capabilities remain false. A live
  support requirement rejects another identity as unsupported and this exact
  identity as hardware-unqualified.
* AKP03 input decoding and JPEG preparation are available. HID discovery and
  diagnostic live transport require the `akp03-hid` feature on Windows or macOS.
  `hardware_qualified` and `command_ready` remain false, including after a
  successful probe.
* Both devices advertise firmware updating as unavailable. CT's firmware-version
  query is diagnostic metadata, not an update or compatibility approval.
* No adapter operation implements keyboard emulation, firmware flashing, stored
  boot-logo changes or direct terminal action delivery.

### Recorded physical evidence

* CT: one guided press/release established raw ID `07` as round button **1/Tab**,
  state `00` pressed, `01` released. This proves neither its LED nor holds,
  repeats, rotation or other controls. All other mappings are reported facts
  awaiting physical verification, except circle/home, which is inferred.
* The Rust macOS ARM64 CT diagnostic completed a zero-duration capture on one
  USB `2ec2:0003` unit reporting firmware `[0,2,8]`: validated HTTP 101,
  28-byte identity response, firmware query and close sent, in 107 ms. No
  display pattern was requested. This establishes that bounded connection/query
  path only, not physical input, visual acceptance or reconnect qualification.
* AKP03 revision 2: one macOS unit demonstrated six upright numbered image tiles,
  volatile-scene clearing, all nine single-button presses/releases, three encoder
  pushes/releases and both encoder directions. The cleared six-tile test used
  34 complete output reports and was visually accepted on that unit only.
* Holding AKP03 button 1 while pressing/releasing button 2 produced no button-2
  report in one observation. Never synthesize missing transitions or claim
  simultaneous-control support.
* Other units/firmware, long-running updates and disconnect/reconnect remain
  unqualified. CT network transport and alternate display-addressing variants
  are unsupported.

## 2. CT receive protocol

The serial convention is asymmetric; it is **not a general WebSocket codec**.
After HTTP upgrade, each incoming frame is:

```text
82 | N:u8 | N payload bytes
```

`N` is 3–255 inclusive. A configured smaller maximum must itself be 3–255.
The default maximum is 255, giving a maximum 257-byte wire frame.

### Incremental framing requirements

1. Retain marker, length and partial payload across arbitrary chunk boundaries.
   Empty input consumes zero bytes and yields no frame.
2. One decoder invocation yields at most one frame plus the consumed byte count.
   The caller explicitly supplies the unconsumed suffix again; there is no
   unbounded queue.
3. A payload byte `82` is ordinary data.
4. An unexpected marker, length below 3 or length above the configured maximum
   faults immediately. Do not scan forward to another marker.
5. After fault, decoding and EOF checking fail until explicit reset. Discard the
   rest of the failed chunk and invalidate its input/connection generation.
6. EOF between frames succeeds. EOF after a marker, length or partial payload
   fails as truncated-frame and faults the decoder.
7. Reset discards buffered data and fault state; it does not establish hardware
   readiness or permit replay of an old-generation prefix.

### Inner input messages

Byte 0 must equal the complete payload length; byte 1 is opcode; byte 2 is the
preserved transaction ID. Every accepted input has an exact length:

| Message | Meaning |
|---|---|
| `04 00 T V` | Tick with uninterpreted byte `V`; not a trusted clock |
| `05 00 T C S` | Button ID `C=00..1a`; `S=00` down, `01` up |
| `05 01 T C D` | Rotation ID `C=00..06`; signed two's-complement byte `D` |
| `09 4d T U Xh Xl Yh Yl I` | Standard-family touch contact |
| `09 6d T U Xh Xl Yh Yl I` | Standard-family touch end |
| `09 52 T U Xh Xl Yh Yl I` | CT-family touch contact |
| `09 72 T U Xh Xl Yh Yl I` | CT-family touch end |

Touch coordinates are unsigned big-endian 16-bit integers. Preserve contact ID
`I` and unknown body byte `U`; out-of-screen coordinates still decode as raw
events. Contact does not distinguish new contact from movement. No tap, gesture,
touch-start or action eligibility is inferred.

Raw rotation accepts every signed byte, including zero and magnitudes greater
than one. Direction calibration and acceleration are absent. Wrong length,
unsupported opcode/control/state or mismatched inner length is an error.
Identity, firmware and display replies are not input events.

## 3. CT controls, touch regions and profile

### Stable raw-control map

There are 27 raw IDs and 15 touch areas, with unique names, addresses and physical
positions. Row and column positions below are zero-based.

| ID | Stable name | Physical position/printing |
|---|---|---|
| `00` | `wheel` | Large bottom-center dial surrounding round screen |
| `01` | `knob_left_top` | Left, top |
| `02` | `knob_left_centre` | Left, middle |
| `03` | `knob_left_bottom` | Left, bottom |
| `04` | `knob_right_top` | Right, top |
| `05` | `knob_right_centre` | Right, middle |
| `06` | `knob_right_bottom` | Right, bottom |
| `07..0e` | `round_1..round_8` | Horizontal row left to right, printed 1–8 |
| `0f` | `button_home` | Left cluster row 0 column 0; circle |
| `10` | `button_undo` | Left row 1 column 0; Undo |
| `11` | `button_keyboard` | Left row 2 column 0; keyboard |
| `12` | `button_enter` | Left row 0 column 1; Enter |
| `13` | `button_save` | Left row 1 column 1; Save |
| `14` | `button_fn_left` | Left row 2 column 1; Fn/lock |
| `15` | `button_a` | Right row 0 column 0; up/A |
| `16` | `button_c` | Right row 1 column 0; left/C |
| `17` | `button_fn_right` | Right row 2 column 0; Fn/lock |
| `18` | `button_b` | Right row 0 column 1; down/B |
| `19` | `button_d` | Right row 1 column 1; right/D |
| `1a` | `button_e` | Right row 2 column 1; E |

Round-button legends in order are Tab, Shift, Ctrl, Alt, Command, Space,
Delete/trash and Backspace.

Capabilities:

* Wheel and six knobs: press/rotate only.
* Round buttons: press and RGB LED.
* Square buttons: press only.
* Screen areas: touch and display, not raw press/rotate/LED.
* Observed evidence applies only to `round_1`'s captured input mapping.
  `button_home` is inferred; other mappings remain reported, not accepted.

### Touch naming

| Area | Rectangle |
|---|---|
| `key_00..key_11` | 4×3 row-major keys, 90×90 each; `x=60+90*(k mod 4)`, `y=90*floor(k/4)` |
| `strip_left` | `x=0..59`, `y=0..269` |
| `strip_right` | `x=420..479`, `y=0..269` |
| `wheel_screen` | `x=0..239`, `y=0..239` |

Rectangle bounds are left/top inclusive, right/bottom exclusive. Diagnostic
wheel naming uses a rectangle, not the graphics circular mask.

Standard-family touch requires contact ID 1–255 and can name only keys/strips.
CT-family touch requires ID 0 and can name only the wheel. Conflicting
family/contact ID, off-screen coordinates or missing capability names no
control. Tick names no control.

### Implemented profile, not the descriptive proposal

Profile identifier: `ct-pid-0003`.

* Every press/touch capability contributes a profile control. Strips are
  touch-slide kind; other press/touch entries are press kind.
* Rotating controls also contribute names ending `_turn`, of wheel kind.
* Recommended intents:
  * `key_00`, `key_01`, `key_04`, `key_05`, `key_08`, `key_09`: select slots 1–6.
  * `wheel_screen`: commit cursor.
  * `round_1`: Stop.
  * `wheel_turn`: move cursor.
  * Other recommendations are absent.
* Event conversion maps raw button states to named Press/Release, and valid
  rotating controls with delta exactly −1/+1 to `<name>_turn`/Turn.
* It drops **all touch**, ticks, zero rotations and other rotation magnitudes.
  Consequently, touch recommendations cannot activate through this adapter.
* The caller supplies connection generation and shared clock sample; both pass
  through unchanged. Transaction IDs do not become action identities.

### Descriptive binding proposal: data only

The following remains unapproved proposal data, not executable dispatch:

* Screen session keys select on release over the same tile; a future recognizer
  must reject slides/multi-touch.
* Round buttons 1–6 select slots on press and mirror status on LEDs, with
  selected LED breathing.
* Wheel turn cycles live sessions with wrap, skipping empty; inside a switcher
  it navigates. Wheel press opens an opt-in switcher and later confirms.
* A/B/C/D navigate up/down/left/right; top-left knob turns provide auxiliary
  up/down; wheel screen displays focused session only.
* Screen commands are Run, Accept, Stop, Fork, Send. Health is display-only.
* Proposed commands still require external guards: Run `/autopilot` and Fork
  `/fork` only idle/empty draft; Accept only one recognized selection and no
  broad approval; Stop sends Ctrl+C once to working/waiting; Send refuses
  empty/slash-command drafts.
* All unlisted controls remain unassigned.

This proposal conflicts with the implemented profile on `round_1`, wheel press
and wheel-screen input. Do not merge the two accidentally.

Documented but unimplemented output commands are `02` LED color, `09` brightness
(reported maximum 10), and `1b` vibration. They are outside the live-probe
allowlist; no parameter encoding or physical acceptance is established here.

## 4. CT pixels and framebuffer addressing

| Display | ID | Dimensions | RGB565 byte order |
|---|---|---|---|
| Center | `A` / `41` | 360×270 | Little-endian |
| Left | `L` / `4c` | 60×270 | Little-endian |
| Right | `R` / `52` | 60×270 | Little-endian |
| Wheel | `W` / `57` | 240×240 | Big-endian |

Convert row-major RGB without rotation, mirroring or headers:

```text
RGB565 = (red >> 3)*2048 + (green >> 2)*32 + (blue >> 3)
```

Discard low bits rather than round. Dimensions must be nonzero and within the
chosen display; exactly `width*height` pixels are required. No truncation/padding.
Bound output allocation to 194,400 bytes and report allocation failures.

Regions, in stable order:

1. Keys 0–11, each center-display 90×90 at
   `(90*(k mod 4), 90*floor(k/4))`.
2. Full left strip.
3. Full right strip.
4. Full wheel.

Reject key indices outside 0–11. A framebuffer preserves placement, region and
caller generation. Prepared bodies are:

```text
opcode 10: 00 | display-ID | x:u16be | y:u16be |
           width:u16be | height:u16be | RGB565 bytes
opcode 0f: 00 | display-ID
```

Preparation performs no I/O. Only `A`/`W` are permitted by the diagnostic pattern;
`L`/`R` remain offline helpers.

Preview decoding expands 5-bit channels as `(c<<3)|(c>>2)` and 6-bit green as
`(c<<2)|(c>>4)`, showing device quantization.

## 5. CT display semantics and exact layout

The renderer accepts six slot views, confirmed/pending selection and action
availability. It does not choose consent, titles, freshness policy or guards.

### Labels and state

* Missing label: dim `Slot N`.
* Masked: dim `Title hidden`; private text must not enter the scene.
* Approved: sanitized/bounded text; blank after sanitation falls back to `Slot N`.
  Unavailable approved labels are dimmed.

| State | Word | Fill RGB | Text RGB |
|---|---|---|---|
| Unassigned | EMPTY | 48,48,48 | 176,176,176 |
| Off | OFF | 97,97,97 | 255,255,255 |
| Idle | IDLE | 255,255,255 | 0,0,0 |
| Working | WORKING | 32,112,255 | 0,0,0 |
| Waiting | WAITING | 255,170,0 | 0,0,0 |
| Complete | DONE | 0,208,96 | 0,0,0 |
| Error | ERROR | 255,48,48 | 0,0,0 |
| Unknown | UNKNOWN | 96,125,139 | 0,0,0 |
| In-flight operation | BUSY | 106,27,154 | 255,255,255 |
| Stale | STALE | 93,64,55 | 255,213,79 |
| Exited | EXITED | 93,64,55 | 255,213,79 |
| Reused process ID | PID REUSED | 93,64,55 | 255,213,79 |
| Unresolved | NOT FOUND | 93,64,55 | 255,213,79 |
| Refusal | REFUSED | 191,54,12 | 255,255,255 |

Color is never the only cue. All text/background pairs must meet 4.5:1 contrast.
Unknown must not appear dark/off. Binding health exited/reused/unresolved takes
precedence over freshness; a stale live binding becomes stale.

Refusal replaces only the band with `REFUSED` plus a second word:

| Reason | Word | Reason | Word |
|---|---|---|---|
| Disabled | DISABLED | Unsupported | UNSUPPORTED |
| Unknown UI | UNKNOWN UI | Stale state | STALE |
| Clock mismatch | CLOCK | Wrong target | WRONG TARGET |
| Changed binding | REBOUND | Changed generation | OUTDATED |
| Changed run | NEW RUN | Changed draft | DRAFT EDIT |
| Changed dialog | NEW DIALOG | Command draft | COMMAND |
| Empty draft | EMPTY DRAFT | Busy | BUSY |
| Broad approval | TOO BROAD | Duplicate | DUPLICATE |
| Indeterminate delivery | UNCERTAIN | Pending input | INPUT WAIT |
| Unavailable | UNAVAILABLE | Unsafe context | UNSAFE |
| Expired intent | EXPIRED | | |

Number, label and selection remain unchanged. The wheel shows focused-slot
feedback too; feedback lifetime is external.

### Selection and transitions

* Confirmed: white solid 3-pixel key frame and `SELECTED`; solid 8-pixel wheel ring.
* Pending: RGB `0,229,255`, dashed frame/ring, `PENDING`.
* Pending wins on its own slot. A different confirmed slot retains its marker.
  Wheel shows pending before confirmed.
* No selection: no key frame; wheel ring `80,80,80`, `NO SELECTION`, en dash.
* Stale transition: assigned slots become stale; unassigned stays empty.
  Clear all feedback and pending, retain confirmed, reset commands unavailable
  and health unknown.
* Neutral/reconnect view: black everywhere except six dim slot numbers.
  No action names, status, selection or wheel content. This is a prepared view,
  not an implemented reconnect transport sequence.

### Default grid and geometry limits

The six session tiles are the **overview**: all six slots remain visible
simultaneously. The wheel is the focused-slot detail view, not a seventh session
or a separate overview protocol. Command/Health tiles occupy the other six keys.

| Row | Column 1 | Column 2 | Column 3 | Column 4 |
|---|---|---|---|---|
| 1 | key 0: slot 1 | key 1: slot 2 | key 2: Run | key 3: Accept |
| 2 | key 4: slot 3 | key 5: slot 4 | key 6: Stop | key 7: Fork |
| 3 | key 8: slot 5 | key 9: slot 6 | key 10: Send | key 11: Health |

Slot-key order `[0,1,4,5,8,9]`; action-key order `[2,3,6,7,10,11]`.
Configurable arrays must collectively contain all 12 distinct indices.
Key inset is 0–6 pixels, default 0. Wheel diameter is 200–240, default 240,
centered in 240×240. Radius is integer floor of half diameter.

Key background is `18,18,18`; normal label/number `240,240,240`; dim `150,150,150`.
For inset `i`, safe rectangle is 90×90 inset `i`; content is a further 4 pixels
inside, so `(x,y)=(i+4,i+4)`, width/height `82−2i`.

* Number rectangle: content x+1, content y, 18×18; font 14 bold, line 18,
  leading alignment.
* Marker: content x+19, content y+3, width content.width−20, height 12;
  font 9 bold, trailing alignment.
* Band: bottom 27 pixels of content, full content width.
* Label area starts content.y+19 and stops 2 pixels before the band. Fit up to
  two 16-pixel lines, centered vertically; x content.x+1, width content.width−2.
  Font 12, centered. At inset 0 this is two lines; larger inset may allow one.
* Band status: font 12 bold, line 16, centered, 1-pixel horizontal margin.
  Refusal uses two font-10 bold lines of height 13.

### Action tiles

Order: Run, Accept, Stop, Fork, Send, Health. Run requests autopilot semantics;
Health has no action.

Create **newly drawn** equivalent semantic symbols: play triangle, checkmark,
stop square, branch/fork, send and heart/pulse. No byte-for-byte asset or SVG
clone is required or permitted by this specification.

| Tile label | Glyph purpose, independently drawn | Semantic action request |
|---|---|---|
| Run | Start/play; clearly distinguish starting work from submitting a draft | Autopilot (`/autopilot`) |
| Accept | Affirm one recognized current selection; check/confirmation motif | Accept |
| Stop | Stop/cancel ongoing work; solid stop motif, not a recording control | Stop |
| Fork | Branch current work into another session; branching motif | Fork |
| Send | Submit current safe draft; directional dispatch motif | Send |
| Health | Controller health/status; heartbeat or diagnostic-health motif | None |

These are **legacy CT presentation labels**, not a new command vocabulary or
authority engine. In particular Run is not a generic primary/submit action and
must not be aliased to Send. The shared main action engine owns intent
validation, guard evaluation, current target/draft/run checks and duplicate
suppression, but implements no delivery. A READY band is a caller-provided presentation hint,
not authorization, and every press needs fresh checks. Accept does not mean
approve all, and Health must never become a synthetic action.

The grid's action-key positions are layout data, not live bindings: the current
CT profile recommends slot selection, cursor movement/commit and Stop on
`round_1`, while its adapter drops touch entirely. Neither drawing an action
tile nor exporting its proposed binding creates a working touch-action route.

There is no required external art pack, bundled icon font, third-party SVG
repository or borrowed-artwork license prerequisite. Draw original glyphs with
equivalent purpose, solid readability and the specified icon-square/coverage
constraints; reuse no existing path geometry. Internal artwork IDs, provenance
text and profile strings may be newly named, provided device identities, wire
bytes, control mappings, semantic actions, layout and externally established
contracts remain equivalent. Embedding original SVG text or generating it from
original primitives are both valid; no network download is part of the build.

| Condition | Word | Fill | Text |
|---|---|---|---|
| Command available | READY | 27,94,32 | White |
| Command unavailable, default | NOT READY | 48,48,48 | 176,176,176 |
| Health unknown, default | UNKNOWN | 69,90,100 | White |
| Health OK | OK | 27,94,32 | White |
| Health warning | WARNING | 249,168,37 | Black |
| Health error | ERROR | 198,40,40 | White |

Icon/name foreground is normal for available command or known health, dim
otherwise. There is no transient action-tile feedback state.

Icon and name sit above the aligned status band. Available vertical height is
`band.y−2−content.y`; icon square side `min(40,height−16−1)`, giving 36 at inset 0
and 24 at inset 6. Center it horizontally/vertically above name with 1-pixel gap.
Name uses font 12 bold, line 16 and 1-pixel content margins.

### Wheel geometry

Center `(120,120)`. Visible radius `floor(diameter/2)`; ring inner radius
`radius−8`; text-safe radius `safe=radius−12`. Scale baseline positive dimensions
by nearest integer `dimension*safe/108`.

| Content | Top relative to center, scaled | Font/line/lines | Maximum width, scaled |
|---|---|---|---|
| Marker | −88 | 12 / 16 / 1 | 140 |
| Number | −70 | 40 / 48 / 1 | 160 |
| Label | −18 | 14 / 19 / 2 | 180 |
| Status pill | +26 | Height 34 | 150 |

For each text rectangle, choose half-width as the smaller of half scaled maximum
and integer square root of `max(0,safe²−dy²)`, where `dy` is the larger absolute
vertical distance of its top/bottom corners. Thus all corners stay inside the
safe circle. Center the rectangle horizontally.

Pill text has 2-pixel side margins. Normal text is font 13/line 18, one centered
line; refusal is font 11/line 15, two centered lines. Draw outside/on visible
circle black last. Both side strips remain black.

### Composition and validation

Each region is an independent bounded canvas. Operations paint in order:
rectangular fill/frame, centered ring, outside-circle mask, text and icon alpha.

* Rectangles must be nonempty and wholly in bounds.
* Frame thickness >0 and twice thickness no greater than the smaller side.
* Ring `inner<outer<=half minimum region dimension`; mask radius within same bound.
* Icon square size 12–90, wholly in bounds.
* Text: nonempty text/rect, at most 1,024 UTF-16 units, font 6–64, line height at
  least font size, 1–4 lines, rectangle height exactly line-height×line-count.
  Alignment is leading/center/trailing; paragraph has explicit LTR/RTL direction.
* Raster coverage must have exactly the requested dimensions and one alpha byte
  per pixel; mismatch fails rather than silently clipping incorrect coverage.
* Blend each channel using integer
  `(foreground*alpha + background*(255-alpha) + 127)/255`.
* Key dashes alternate 6 pixels on/off; measure x on horizontal edges, y on
  vertical edges. Wheel dashes alternate 32 angular segments, 16 on/16 off.
* Radial geometry uses pixel-center distances.
* Changed-scene detection compares full content including region/background.
  Return next scenes absent from previous content. No transport cache/scheduler
  is implied.
* Same scene/rasterizer/generation yields identical output. System-font output
  need not be identical between machines.

### Completion acknowledgment, redraws and rate limiting

The offline renderer is synchronous: success means only that validated scenes,
coverage and RGB565 buffers have been prepared. Prepared generation metadata,
an unchanged-scene comparison or successful PNG encoding does not acknowledge
delivery to a device.

The diagnostic CT pattern waits for the matching opcode/transaction reply after
each draw and refresh before issuing the next command. Exactly three reply bytes
acknowledge that command exchange, **not** physical visible completion. A
successful report therefore says `correlated_replies_only` and retains
`visual_acceptance=unqualified`. A close write has no acknowledgment at all.
AKP03 full write success likewise proves report acceptance only; the display
probe neither waits for an image acknowledgment nor interprets captured ACK
reports as visual completion.

There is **no implemented numeric live-display frame rate, minimum redraw
interval, animation loop, retry/backoff scheduler or production completion-
acknowledgment state machine in these device helpers**. Changed-scene detection
is a pure region comparison, not a rate limiter. CT bounds command work using
one in-flight query, per-I/O/total deadlines and finite byte/event budgets;
AKP03 bounds capture/report counts and uses its five-second initialized
keep-alive, not a display-frame timer. Native CT completion polling at 5 ms
and its 500 ms cancellation grace are resource-lifetime controls, not display
update intervals.

Any package-wide presentation scheduler, output coalescing, completion ledger,
per-device update budget or backpressure would require a separately specified
future contract; these are not additional reconstruction requirements. Such a
future contract must not infer visible success from serialization or a transport
write, replay uncertain writes, or convert an unqualified diagnostic reply into
hardware qualification. No absent rate-limit value should be guessed.

### Display text preparation

This is presentation sanitation, **not redaction** or command-guard input.

1. Scan at most 256 Unicode scalar characters; detect additional input.
2. Control and separator categories become collapsed whitespace; remove leading/
   trailing whitespace.
3. Remove formatting characters including bidi overrides, isolates and BOM.
   Preserve attached ZWJ/ZWNJ only when prior text exists and whitespace is not
   pending. Record substitution for removed formatting.
4. Private-use/unassigned/surrogate categories become U+FFFD.
5. Approximate clusters attach combining marks, joiners, emoji modifiers,
   successors of ZWJ and regional-indicator pairs. Bound joiner/modifier chains
   to 16 characters, then force a new cluster. Keep at most 4 combining marks
   per cluster. No full Hangul-jamo/Indic-conjunct segmentation is promised.
6. Keep at most 40 clusters; overflow retains at most 39 plus U+2026. Trim spaces
   before ellipsis. Requested limit zero behaves as one. Exceeding scan cap
   appends ellipsis to nonempty sanitized text even if fewer clusters remain.
7. Return text, truncated/substituted flags and base direction.

Base direction is determined by the first Unicode letter. RTL ranges are
`0590..08ff`, `fb1d..fdff`, `fe70..feff`, `10800..10fff`, `1e800..1efff`;
otherwise LTR, including no letter. Each label is a separate paragraph.

### Native text, graphics and PNG

The provided native raster path is Windows-only:

* Default font Segoe UI, system fallback, locale `en-us`. No bundled fonts.
  Custom family must be nonempty, no NUL, at most 64 UTF-8 bytes.
* Per-instance isolated text factory, single-thread graphics factory, own image
  factory and balanced COM reference; tolerate an existing STA. Instances remain
  bound to their creating thread; no process-global cache.
* Software 96-DPI transparent premultiplied BGRA, grayscale text antialiasing,
  normal/bold weight, normal style/stretch. Rendering gamma 1.8, enhanced
  contrast 0.5, subpixel contribution 0.0, flat pixels, natural-symmetric mode.
* Center vertically; wrap only multi-line text; character ellipsis trimming;
  clip to rectangle. Uniform baseline approximates
  `size*1.08+(line-height−size*1.33)/2`, clamped to the line.
* Native SVG rendering requires Windows 10 version 1703 or later and its
  corresponding interface. Missing interface fails with native API/HRESULT,
  no fallback. Independently authored symbols render to alpha and are tinted
  by compositor, not intrinsic SVG fill.
* PNG output: nonzero dimensions ≤4096 each, exact RGB pixel count; convert to
  24-bit BGR and commit PNG through OS codec. Reject changed pixel format.
  Caller chooses the output path.
* Other OSes may use the pure compositor with an injected backend, but no
  non-Windows native text backend is included in this device scope.

### Independent bundled status SVGs and hash manifest

The package's shared bundled artwork is separate from the six CT
**action** glyphs above. Independently draw one for each of these six status
semantics, as required by the main brief:

| Status artifact | Required meaning | Possible original visual treatment |
|---|---|---|
| Idle | Available and not currently working | Quiet/ready motif |
| Working | Active work in progress | Activity/progress motif |
| Waiting | Needs attention or a decision | Waiting/attention motif |
| Complete | Work completed successfully | Completion/check motif |
| Error | Work failed or encountered an error | Error/alert motif |
| Unknown | Current state cannot be established; not Off | Uncertainty/question motif |

The suggested treatments describe purpose, not reusable SVG geometry. Human
status words and color must remain consistent with the shared presentation
contract; CT rasterization uses the state palette in this appendix. Off,
empty, unavailable, busy, refusal and selection effects remain separately
distinguishable and must not silently collapse into one of the six glyph states.
The six files are not the six action icons and are not PNG preview exports.

Bundle self-contained valid SVG files with original shapes, explicit dimensions/
viewBox and no external fonts, linked images, scripts or network references.
Preserve the package's logical status-to-artifact mapping and stable semantic IDs
such as `state-idle`; original filenames can be chosen independently. A hash manifest associates
each status and bundled filename with the SHA-256 of that file's **actual final
bytes**. Use deterministic encoding/newlines/serialization, hash after final
serialization and keep builds deterministic for the same artwork inputs.
No previously published digest or asset byte sequence is a compatibility
requirement: original artwork necessarily has its own digests. A verifier
recomputes each digest, checks exactly one artifact for every expected status,
and rejects missing, duplicate or altered artifacts. Manifest layout is an
internal choice, not a new public JSON contract.

Do not add an artwork-export command. The CT examples do not create these
bundled status SVGs or their manifest. No copied art pack, asset notice or asset
license is needed to author them.

### Offline previews and examples

These are two independent **offline example/export surfaces**, not live
diagnostic commands. Their entrypoint identifiers are `ct_control_map` and
`ct_tile_previews`. The first exports control-map data/documentation; the second
exports rendered PNGs. Neither enumerates, opens, probes or writes any hardware,
updates firmware, injects terminal input, or establishes hardware acceptance.

* Overall base image 480×526: left/center/right across y=0..269; wheel origin
  `(120,286)`, size 240. Backdrop `64,64,64`. Integer scale 1–4; nearest-neighbor
  expansion only.
* Display/per-buffer previews are native size. Uncovered pixels retain backdrop;
  overlaps paint in supplied order. Reject malformed buffers/outside placement.
* Tile-preview arguments: zero or one output-directory argument. No user-supplied
  state, label, device selector, image scale or hardware flag. A second positional
  argument fails. Native rasterizer creation occurs before Windows argument
  validation; rasterizer initialization failure can therefore precede usage error.
* Tile-preview rendering is Windows-only; another OS prints a requirement message
  to stderr and exits 2 without producing previews. Windows requires the native
  font/graphics/PNG services above, including the SVG interface for action icons.
* With a directory argument: create it recursively if needed; overwrite 16
  native-size PNG files: `key-00.png` through
  `key-11.png`, `center.png`, `left-strip.png`, `right-strip.png`, `wheel.png`.
  Dimensions are respectively 90×90, 360×270, 60×270, 60×270 and 240×240.
  Use the mixed-state scenario below and framebuffer generation 1.
* With no argument: require nonempty `CARGO_TARGET_DIR`, create `ct-previews`
  underneath, overwrite these 11 PNGs at 960×1052 (2× overview scale):

  ```text
  01-mixed-states.png
  02-selection-confirmed-and-pending.png
  03-long-and-unicode-labels.png
  04-privacy-masked.png
  05-refusal-feedback.png
  06-stale.png
  07-neutral-reconnect.png
  08-live-statuses.png
  09-other-states.png
  10-inset6-wheel200.png
  11-awaiting-approval.png
  ```

* Each saved PNG path is printed on its own stdout line. Rendering, directory,
  environment or write errors propagate; output creation is not transactional,
  so already written images may remain after a later failure. Pixel count and
  file-size equality across system-font versions are not promised.
* No separate image, HTML, JavaScript or manifest file is implicitly created by
  tile-preview export. It does not update control-map documentation.
* Example availability is synthetic, never a substitute for guard evaluation.

The fixed native-size mixed scenario has:

| Slot | Label policy/content | State | Feedback |
|---|---|---|---|
| 1 | Approved `build-agent` | Working | None |
| 2 | Masked | Waiting | None |
| 3 | None | Unassigned | None |
| 4 | Approved `tests` | Busy | None |
| 5 | Approved `docs` | Exited | None |
| 6 | Approved `deploy` | Error | Refused / stale state |

Confirmed slot1, no pending selection; Run/Stop/Fork/Send READY, Accept NOT READY,
Health WARNING. The waiting-approval scenario confirms slot2, offers only
Accept/Send and shows Health ERROR. Confirmed/pending gallery uses confirmed2,
pending5, and clears slot6 feedback. Stale derives from that scenario. Privacy
gallery alternates Masked/None labels, all Working, confirmed1. Refusal gallery
uses six Waiting slots and, in order, command draft, wrong target, indeterminate
delivery, broad approval, changed generation and unsafe context; confirmed3.
Live-status gallery uses Off/Idle/Working/Waiting/Complete/Error, confirmed3;
other-state gallery uses Unknown/Busy/Unassigned/Exited/PID-reused/Unresolved,
confirmed2. Unicode/long-label gallery uses the approved-text sanitation cases
and confirmed4; the inset6/wheel200 gallery applies reduced geometry to it.
These strings are synthetic samples, not harvested session titles.

## 6. CT bounded diagnostic transport

### Options and deadlines

Probe kind `ct_serial_probe`, schema version 1. Options reject unknown JSON
fields and accept no arbitrary port, command, file, label or persistent setting.

| Option | Default | Range |
|---|---:|---|
| `schema_version` | 1 | Exactly 1 |
| `total_timeout_ms` | 8000 | 1000–8000 |
| `io_timeout_ms` | 1000 | 100–1500, ≤total |
| `capture_ms` | 2000 | 0–5000, ≤total |
| `max_events` | 128 | 1–128 |
| `max_received_bytes` | 32768 | 256–32768 |
| `test_pattern` | false | Boolean |

Invalid options produce no I/O. Deadlines use injected monotonic elapsed time;
clock regression fails. Cancellation is a one-way shared flag.

### Explicit CLI integration

`ct-probe` accepts only `--schema-version`, `--total-timeout-ms`,
`--io-timeout-ms`, `--capture-ms`, `--max-events`, and
`--max-received-bytes`, each followed by an unsigned ASCII decimal value, plus
the valueless `--test-pattern` opt-in flag. Defaults and cross-field bounds are
the table above. Unknown, repeated, missing-value and invalid options fail before
worker launch; arbitrary ports, commands, paths and selectors are not accepted.
Neither `doctor` nor inventory invokes this route.

The coordinator and hidden worker share a compiled static diagnostic allowlist
containing `health`, `ct_serial_probe`, `micro_hid_probe`, and
`akp03_hid_probe`; the health coordinator retains health-only authority.
CT's callback validates the object payload before
calling `devices::ct::probe::run_native`; only the contained disposable Windows
worker owns Windows serial handles, overlapped storage and the mutex. On
macOS ARM64, serial discovery and access require private CT-specific authority
issued inside the validated same-executable process-group worker (§8.7 of the
platform appendix). Health, HID callbacks and ordinary in-process callers do not
receive CT authority. The Windows callback also checks detached, one-process
kill-on-close Job containment using the existing health diagnostic before native
access, refusing a direct uncontained hidden worker invocation even with valid
options. Both sides
validate typed reports/failures, fixed privacy-safe strings, qualification and
request-specific bounds. Framed JSON rejects duplicate keys before conversion
to a generic value. No vendor executable, dependency or device code is used.

Supervision allows `total_timeout_ms + 500 + min(io_timeout_ms,250) + 500
+ 1000` milliseconds: the session, terminal cancellation grace, separate close,
its possible terminal grace, and a bounded startup/response/exit allowance.
This is at most 10250 ms, not an unlimited native-driver wait. Existing worker
failure cleanup can additionally wait up to 5 seconds for exit and 1 second for
I/O joining; unresolved cleanup retains its existing fail-closed fence.

A successful CLI report has `command_ready: false` and a typed `ct_probe` report.
Native failures travel inside the worker's correlated transport payload as
`status: probe_error` and become nonzero CLI failures, not successful empty
reports. Worker failures (including non-Windows `unsupported_platform`) have
`status: worker_error`. Errors go only to stderr; no success JSON is printed.
Options failures have `status: probe_error`, kind `invalid_options`, stage
`options` and no progress. The private `__probe-worker` route never falls through
to ordinary command execution. Windows cross-compilation does not verify real
worker containment, driver cancellation or physical hardware acceptance.

### Windows selection and serial setup

Native diagnostics require a disposable contained helper. Other OSes fail as
unsupported platform except for the macOS ARM64 path below. Cancellation before
opening fails immediately.

1. Acquire a non-inheritable per-user local named mutex preventing concurrent CT
   probes. Busy or abandoned ownership is refused; release abandoned ownership.
2. Require discovery to establish unique selection, exactly one matching CT
   interface, at least one present serial interface, exactly one port, selected
   port equal to that sole port and valid normalized COM name. Distinguish not
   found, incomplete and ambiguous.
3. Open the selected existing COM object exclusively, read/write, overlapped,
   non-inheritable. Re-enumerate association after open; changed port fails.
4. Read settings; DTR/RTS must each be static off/on modes 0/1. Reject handshake/
   toggle modes 2/3 instead of silently changing them.
5. Set 256000 baud, 8 data bits, no parity, one stop bit, binary mode, no flow
   control, substitution or NUL stripping; preserve static DTR/RTS and never pulse.
   Read back/verify settings. Mode evidence does not measure electrical level.
6. OS read mode returns available bytes immediately or waits ≤50 ms for first
   byte; OS write timeout 1000 ms, with separate protocol deadlines.
7. Subtract preparation time from total budget. Require at least 1000 ms remaining
   and capture within remainder. Otherwise configuration-stage timeout.

### macOS ARM64 selection and serial setup

This is an explicit diagnostic extension, not a production controller or a
network-transport fallback. Use the OS-provided CDC ACM serial endpoint for the
connected USB `2ec2:0003` device; never guess a port or accept a user-supplied
device path.

* Discover through IOKit. Require exactly one matching USB device and one
  associated serial callout endpoint. Missing, incomplete or ambiguous evidence
  fails closed. Retain native registry identities and verify the association
  again after opening.
* Keep cooperative ownership and OS serial exclusivity for the lifetime of the
  open descriptor. Open only an existing character device, nonblocking,
  close-on-exec, without a controlling terminal or symlink traversal.
* Configure raw 8N1 with flow control disabled and nominal 256000 baud using
  the native serial-speed API. Do not pulse DTR/RTS, discard incoming data or
  claim electrical line-level evidence. Report only settings the native API
  actually verifies; `serial_settings` remains null on macOS because its API
  does not establish the Windows static DTR/RTS mode contract.
* Bound nonblocking reads and writes by the same protocol deadlines and
  cancellation checks. Set and verify `VMIN=1`, `VTIME=0` so an idle nonblocking
  tty read yields `EAGAIN`, not a zero-byte result mistaken for disconnect.
  Handle partial writes by exact suffix; never retry an uncertain native write.
  Device removal, association changes, deadline expiry and cancellation are
  explicit failures.
* Keep descriptors and ownership inside the disposable worker. No blocking
  drain, reconnect or in-process fallback may escape the supervision budget.
  macOS process-group/watchdog limitations remain those in platform §8.7.

The upgrade, query, capture, optional pattern, close, parsing and privacy
contracts below are shared unchanged. The default probe sends protocol
handshake/query/close bytes; it is **not** a zero-write probe. Display mutation
still requires `--test-pattern`, and neither success nor an ACK is physical
input/display qualification.

### Session sequence and wire output

Write this exact LF-only request:

```text
GET /index.html\nHTTP/1.1\nConnection: Upgrade\nUpgrade: websocket\nSec-WebSocket-Key: 123abc\n\n
```

Upgrade response:

* Maximum 1,024 header bytes, terminator included; LF-LF or CRLF-CRLF.
* Only ASCII graphic bytes, space, tab, CR/LF.
* First two whitespace-delimited tokens exactly `HTTP/1.1` and `101`; reason
  phrase not validated.
* Exactly one case-insensitive `Upgrade` with trimmed `websocket` value, and
  exactly one `Connection` containing comma-delimited `upgrade` token.
* Every other nonempty line must contain colon. Other headers allowed.
* No accept-key verification. Keep coalesced post-header bytes for serial parser.
* Initial `82` or `88` means suspected stale session: stop immediately, no more
  writes or in-run retry; operator replug required.
* Other binary first byte is invalid handshake. Incomplete header times out.

After validated upgrade:

1. Identity query opcode `03`, transaction 1, empty body. Reply body 1–64 printable
   ASCII `20..7e`, not all spaces. Preserve only body length.
2. Firmware query opcode `07`, transaction 2, empty body. Body 3–32 bytes;
   preserve first three version bytes only.
3. Optional pattern: draw opcode `10`, transaction 3, display A; refresh `0f`,
   transaction 4; draw W, transaction 5; refresh W, transaction 6.
4. Capture for requested duration starting after preceding work. If full duration
   would exceed total deadline, fail instead of shortening it. Earlier interleaved
   input already counts against event budget.
5. Attempt one safe close sequence.

Outgoing command framing is deliberately different from receive framing. Let
`M=3+body-length`:

```text
M <= 255:
82 | wrapping_u8(80+M) | 00 00 00 00 |
min(M,255) | opcode | transaction | body

M > 255:
82 ff | 00 00 00 00 | M:u32be | 00 00 00 00 |
ff | opcode | transaction | body
```

Do not replace this with a standard network-framing implementation.

Exact identity request: `82 83 00 00 00 00 03 03 01`.
Firmware request: `82 83 00 00 00 00 03 07 02`.

Pattern body is `00,ID,0:u16be,0:u16be,48:u16be,48:u16be,pixels`.
48×48 pixels: one-pixel white border; interior top-left red, top-right green,
bottom-left blue, bottom-right black. Body 4,618 bytes, message 4,621 bytes,
long prefix 14 bytes. It does not clear whole displays and may remain visible.

### Query/read/write behavior

* One per-I/O deadline covers complete request write plus reply wait.
* Positive partial writes continue only at exact suffix. Zero write is
  short-write; count greater than requested is contract failure. Native error
  is never retried because transferred prefix is uncertain.
* Same transaction but wrong opcode fails unexpected-reply, even for an
  otherwise-valid input message.
* Reply opcodes `03/07/10/0f` with another transaction fail unexpected-reply.
  No duplicate or stale response resolves a query.
* Other messages must strictly decode as input. Invalid input fails the session.
* Reply outer payload length is authoritative. Inner declared length smaller than
  actual fails; larger is accepted and counted as a disagreement. This exception
  applies only to correlated replies, never input.
* Draw/refresh replies require exactly three payload bytes, no status body.
* Read buffer 512; capacity limited to remaining receive budget. `Data(0)` or
  count beyond capacity is contract failure. Explicit idle is finite timeout.
* Recheck time/cancellation after native read before accepting bytes. Diagnostics
  may count completed late bytes, but those bytes are not valid input.
* Disconnect checks framing EOF first; partial frame reports truncated-frame.
* Receive budget includes upgrade/replies/input. Exhaustion is byte-limit error.
* Ticks count separately, not sample storage. Max events during capture returns
  successful `input_limit_reached`; an extra input while awaiting query fails
  input-limit. At capture deadline, partial frame fails truncated-frame.
* An injected transport clock must advance during idle reads; a frozen clock
  cannot satisfy bounded-time behavior.

### Close, cancellation and pending operations

Close bytes: `88 80 00 00 00 00`, no acknowledgment read.

* Never close without this run's validated upgrade.
* Skip after cancellation, disconnect, cleanup pending, clock regression,
  transport-contract failure, cancelled flag or uncertain/partial prior write.
* Otherwise close once even after an ordinary timeout/invalid reply.
* Separate close budget `min(io_timeout,250 ms)`, starting after session; it may
  exceed exhausted session deadline. Continue positive suffix writes, but never
  retry a failure or begin another close sequence.
* Close outcome: not attempted, skipped, sent, write failed. Sent is transport
  acceptance only. Close failure does not replace primary outcome.
* Written-byte totals include accepted close bytes. Synthetic session elapsed
  is measured before close; native wrapper elapsed includes wrapper completion.

Native overlapped storage/handles/lease remain owned until terminal completion.
On cancellation/deadline, request cancellation once, then poll every 5 ms for up
to another 500 ms. A racing successful completion still returns the interruption,
never cancelled data. If still pending, return cleanup-pending, quarantine all
operation storage/handles/lease until helper exit and poison the transport.
No replacement operation or reconnect in that process.

After every native completed read, including zero-byte idle, inspect line status
before copying bytes. Any nonzero line flags fail; OR flags cumulatively and
retain last successful queue lengths. Access denied/sharing violation maps to
busy/denied; device-not-connected to disconnected; other errors to native failure.

### Report and error contracts

Successful CT report includes:

* Schema 1; evidence synthetic or native transport observation.
* Hardware, physical-input and visual qualification always unqualified.
* HTTP-101 flag; identity response length; firmware three bytes.
* Reply-length-disagreement count; optional serial settings.
* Pattern not-requested or correlated-replies-only.
* Completion capture-duration-elapsed/input-limit-reached.
* Elapsed/received/written counts, close outcome, tick count.
* Input samples with helper-local elapsed milliseconds and exact strictly decoded
  inner payload. These times are not shared action timestamps.

Do not expose serial number, port path, USB strings, raw identity/version reply
or unknown bytes. Errors retain kind, stage, optional numeric native code and
optional progress; no raw OS text or identity.

Error kinds: `invalid_options`, `unsupported_platform`, `device_unavailable`,
`discovery_incomplete`, `ambiguous_device`, `association_changed`,
`busy_or_denied`, `native_failure`, `invalid_serial_settings`, `serial_line_error`,
`cancelled`, `timeout`, `disconnected`, `invalid_handshake`,
`suspected_stale_session`, `malformed_frame`, `truncated_frame`, `unexpected_reply`,
`invalid_reply`, `invalid_input`, `input_limit`, `byte_limit`, `short_write`,
`transport_contract`, `clock_regression`, `cleanup_pending`.

Stages: options, discovery, open, configure, handshake, identity, firmware,
pattern, capture, cleanup. Remap transport errors to current stage except pending
cleanup.

Progress includes highest phase, nonregressing elapsed timing, open/configured
milestones, accepted/total request-byte counts, received counts, first-response
class, header bytes/completed/validated, frame counts and close status.
Phase order:

```text
preparing, discovery, open, association_check, configure, upgrade_write,
upgrade_response, header_validation, identity_write, identity_response,
firmware_write, firmware_response, pattern, capture
```

Only strictly increasing phases append; timeline ≤14 entries. Preserve last
protocol phase on cleanup. First-response classes are text, binary-frame,
close-frame, other-binary. Cached serial diagnostics contain calls/failures/
completed bytes, idle reads, line checks/failures/error observations/ORed flags
and last queue lengths. Reading cached diagnostics must not issue transport I/O.
Structured report/error/progress records reject unknown fields.

### Exact typed failure-progress fields

Failure envelope fields are `kind`, `stage`, optional unsigned `native_code`,
and optional `progress`. A reader may default an omitted `progress` to absent
for older messages; other unknown fields still fail. Numeric native error data
is not permission to append raw OS text.

`progress` has these exact field groups:

| Fields | Meaning |
|---|---|
| `evidence` | Optional `synthetic` or `native_transport_observation` |
| `last_phase`, `elapsed_ms` | Highest entered phase, helper-local elapsed time |
| `port_opened_at_ms`, `port_configured_at_ms` | Optional completed milestones, not merely attempted phases |
| `phases` | Records `{phase,started_ms,elapsed_ms}`, ordered, at most 14 |
| `upgrade_request`, `identity_request`, `firmware_request`, `close_request` | Each `{written_bytes,total_bytes}`; accepted bytes, not attempted bytes |
| `upgrade_response_bytes` | Bytes received while waiting for upgrade, including coalesced binary frames |
| `handshake_response_start` | Optional `text`, `web_socket_binary_frame`, `web_socket_close_frame`, `other_binary`; never raw first byte |
| `header_bytes`, `header_complete`, `header_validated` | Separate framing and semantic-validation evidence |
| `received_bytes`, `frames_received` | Total received bytes and completed frames |
| `identity_frames_received`, `firmware_frames_received` | Correlation progress, not exposed reply content |
| `session_close` | `not_attempted`, `skipped`, `sent` or `write_failed` |
| `serial` | Optional cached native-I/O diagnostics |

Request totals are upgrade-request byte length, identity9, firmware9 and close6.
Progress begins at preparing/time0. Time observation uses saturating subtraction
and nondecreasing maximum; elapsed milliseconds saturate at unsigned32 maximum.
The current phase's duration updates even on error. Re-entering the same/lower
phase never adds another timeline record; a failing phase remains identifiable
without a misleading success/cleanup phase.

`serial` fields are:
`read_calls`, `write_calls`, `read_failures`, `write_failures`,
`completed_read_bytes`, `completed_write_bytes`, `idle_reads`,
`line_status_checks`, `line_status_failures`, `line_error_observations`,
`line_error_flags`, optional `last_input_queue_bytes` and
`last_output_queue_bytes`. Keep flags cumulatively ORed. A call failure does not
become a successful completed byte count.

Examples of observable partial failure: open failure has no opened milestone;
association failure can have opened but not configured; invalid HTTP can have
complete but unvalidated header; identity timeout can have full request9 and
zero identity frames; partial write distinguishes accepted prefix from total.
Close progress may change while `last_phase` remains identity/firmware/capture.
No single `success=false` boolean can replace this evidence.

## 7. AKP03 input, identity and profile

Input is one complete HID report, with no byte-stream reassembly or automatic
leading-ID removal. Original accepts 10–512 bytes, revision 2 accepts 11–512.
Measured revision-2 input begins `ACK` directly, with no report-ID byte.

* Bytes 0–2: ASCII `ACK`.
* Byte 9: control code.
* Byte 10: explicit push state on revision 2 only.
* Bytes 3–8 otherwise unconstrained except acknowledgment recognition.

### Acknowledgment precedence

Prefix `ACK 00 00 OK` at offsets 0–6:

* Original: acknowledgment regardless of nonzero code, preserving its legacy
  decoding distinction.
* Revision 2: acknowledgment only when code is zero. Nonzero codes remain controls.
* Code zero without that prefix: no-control diagnostic, not a gesture.

| Codes | Logical control |
|---|---|
| `01..06` | Buttons 0–5, display keys |
| `25`, `30`, `31` | Buttons 6, 7, 8 |
| `33`, `35`, `34` | Encoder push 0, 1, 2 |
| `90/91` | Encoder 0 turn −1/+1 |
| `50/51` | Encoder 1 turn −1/+1 |
| `60/61` | Encoder 2 turn −1/+1 |

Original button/push reports are repeated **press observations**, never toggles
or fabricated release pairs; ignore byte 10. Revision-2 push state accepts only
0 released/1 pressed. Turn codes ignore byte 10. One control report never clears
other held controls. Unknown code, header, length or explicit state fails.

Hex input accepts 20–1,024 characters, even length, only ASCII hex digits in either
case, no separators/whitespace. Parse one report then enforce profile length;
20-digit revision-2 hex is syntactically valid but too short a report.

Serialized input uses `kind` and snake-case variants: acknowledgment, no_control,
button_press_observed(index), encoder_press_observed(index),
encoder_turn(index,delta), button_state(index,pressed), encoder_state(index,pressed).

### Physical and profile mapping

Observed revision-2 keys 0–5 correspond to digits 1/2/3 top, 4/5/6 bottom.
Extra buttons 6–8 are left-to-right. Encoder 0 is bottom-left; 1 is the large
top knob; 2 bottom-right. Negative is counterclockwise, positive clockwise for
the observed unit. No acceleration, debounce, hold or repeat policy.

Profile identifiers: `akp03-original`, `akp03e-rev2`.

* `button_0..button_8`: press kind. Buttons 0–5 recommend slots 1–6.
* `button_8`: Stop, isolated-stop true, near-selection false, adjacent only to
  `button_7`; button 7 records reciprocal adjacency.
* Every encoder contributes `encoder_N_turn` and `encoder_N_press`.
  Encoder 1 recommends cursor motion/commit; others have no recommendation.
* Convert valid explicit button/encoder states to Press/Release; valid turn
  index and ±1 to Turn. Drop press observations, ack/no-control and invalid
  indices/deltas. Original turns can convert; original pushes cannot.
* Caller supplies generation/shared timestamp, propagated unchanged.

The older preview's “Unbound” extras are a separate logical surface, not this
newer profile's recommended mapping.

## 8. AKP03 JPEG, wire output and logical previews

Every HID write is 1,025 bytes: report-ID byte 0=`00`, followed by 1,024 payload
bytes. Start zero-filled. Command skeleton has ASCII `CRT` at 1–3, zero at 4–5,
opcode beginning at 6.

| Command | Bytes | Extra field |
|---|---|---|
| Initialize | `DIS` at 6–8 | None |
| Brightness | `LIG` at 6–8 | Percent at 11, 0–100 inclusive |
| Keep alive | `CONNECT` at 6–12 | None |
| Flush | `STP` at 6–8 | None |
| Clear volatile scene | `CLE` at 6–8 | Byte 12=`ff` |

Unspecified bytes remain zero. Clear is not a stored-logo operation.

### Image encoding

* Tightly packed row-major interleaved RGB, exactly 10,800 bytes original
  60×60 or 12,288 bytes revision-2 64×64.
* Original: copy unchanged. Revision 2: source `(x,y)` becomes
  `(size−1−y,x)`, clockwise 90°.
* Encode JPEG RGB quality 90. Fail encoder error or length >65,535.
* Raw JPEG entry checks only length 4–65,535, `ff d8` start and `ff d9` end,
  not syntax/dimensions. Use trusted JPEG or exact-size RGB path.
* Key must be 0–5; validate key before RGB encoding.

Image transaction:

1. Standard `BAT` command report: length `u16be` at 11–12, key+1 at 13.
2. `ceil(length/1024)` raw reports: ID 0 then next image chunk, last tail zero
   padded. No CRT header, sequence number or checksum in raw chunks.
3. One `STP`.

Total `ceil(length/1024)+2` reports. Do not add a firmware/`LOG` command.

### Diagnostic tile geometry

Key background colors in order:
`(160,25,25)`, `(25,145,25)`, `(25,25,160)`, `(20,130,130)`,
`(140,20,140)`, `(150,130,20)`.

Draw logical tile before transport rotation:

* Black inset strip: x 17 through size−18, y 3 through size−4.
* White upward arrow: y 5–12, inclusive x center−(y−5) through center+(y−5);
  stem x center−2 through center+2, y 12–19.
* Digit 1–6: 3×5 mask, each set cell 5×5, origin `(center−7,26)`, MSB left.
  Row masks:
  * 1: `[2,6,2,2,7]`
  * 2: `[7,1,7,4,7]`
  * 3: `[7,1,7,1,7]`
  * 4: `[5,5,7,1,1]`
  * 5: `[7,4,7,1,7]`
  * 6: `[7,4,7,5,7]`

### Distinct logical SVG previews

Neither preview is a transported device framebuffer or applies JPEG rotation.

**Legacy six-slot preview:** 260×310, six 60×60 keys at
`(24+76*(i mod3),38+76*floor(i/3))`. Labels use sanitation capped at 10 clusters
and XML escaping of `& < > " '`. Missing is `Slot N`, masked is `Masked`.
Status/refusal band uses CT palette; selection border uses marker or gray.
Three extra buttons/encoders say Unbound; output declares no device I/O or
terminal actions. It does not render detailed refusal reason.

**Profile preview:** six realized slots, equal size 60 or 64 only; missing screen
geometry or unequal/unsupported sizes fail. Canvas 240×190; key origins
`(10+76*(i mod3),10+85*floor(i/3))`. Shows semantic glyph, style/status word and
`Slot N`, no private titles. Selection determines border/dashing; otherwise static
emphasis uses fill, default gray. Border width 4 adds white word plate/black text;
other widths have no word plate. Uses shared token/artwork/effect semantics.

Legacy demo statuses: idle, working, waiting, complete, error, unassigned.

## 9. AKP03 discovery and bounded probe

### Metadata discovery

Requires `akp03-hid` on Windows/macOS; otherwise discovery-unavailable.
Enumerate VID `0300` products `1001..1003` and `3002..3003`.
Return vendor/product, page/usage, interface number, supported flag and optional
original/rev2 profile. Only `1001`/`3002` with `ffa0:1` are supported.

Discovery does not open report transport, send feature queries/initialization,
select first match, or expose serial numbers/paths/USB strings. Enumeration may
transiently open metadata handles; it is not guaranteed handle-free scanning.

### Live selection and descriptor

Probe only `0300:3002`, page `ffa0`, usage 1, interface 0. Require exactly one
candidate. Open by path, re-read and compare vendor/product/page/usage/interface/
path. Any mismatch fails.

Read descriptor into a 1,024-byte buffer and require byte-for-byte equality:

```text
06a0ff0901a1010902a10006a1ff090309041580257f350045ff75089600028102090509061580257f350045ff75089600049102c0c0
```

It describes 512 input bytes, 1,024 output payload bytes, no feature reports.
Reject changed/equivalent-but-different, empty, keyboard or nested-collection
descriptors before reports.

### Options and sequencing

* CLI capture 0–30,000 ms, default 2,000; initialize false.
* Test-pattern requires initialize. Health check conflicts with both.
* Validate programmatic options again. The helper's zero/default value has
  capture 0, distinct from CLI's 2,000.
* Health check must not construct transport or enumerate/open hardware.

Passive capture writes nothing, including no keep-alive.

Initialized sequence:

1. Phase initialize: `DIS`, `LIG` at 50.
2. Set initialized true after those two full writes.
3. `CONNECT`. Its failure can leave initialized true while outcome fails.
4. Optional phase display: `CLE`, `STP`, then six revision-2 diagnostic key
   transactions in key order. Increment patterns-written only after entire key.
5. Phase capture: deadline elapsed+capture duration; first additional CONNECT
   five seconds after capture starts, then five seconds after previous one,
   only when initialized output was requested.

There is no acknowledgment pacing, sleep between writes, firmware read,
initialization reply handshake or keep-alive during display output.

Each write must return exactly 1,025. Count reported bytes even on short write,
but increment successful report count only for complete writes. Any short/error
stops immediately with no retry/suffix write.

Capture:

* Read buffer 513, detect >512 as fatal.
* Timeout `min(50 ms,remaining capture duration)`.
* Zero bytes is idle.
* Every nonempty ≤512 report preserves elapsed milliseconds, byte count,
  lowercase raw hex and decoded revision-2 input or textual decode error.
* Unknown/malformed report is retained, not a whole-capture failure.
* All reports count toward 128, including ack/errors. At bound, set truncated
  when the next loop begins before deadline. Never invent releases.
* Success sets phase complete/elapsed; failure retains current phase and partial
  progress. No brightness restore, scene restore, shutdown report or final clear.
  Display/brightness may remain changed after handle release.

### Process host and result

Use same-executable hidden worker `__akp03-worker` with explicit capture duration
and selected flags. Only child owns HID handles. Parent timeout is
capture+15,000 ms, health-check timeout 3,000 ms. Kill/reap stuck child. This is
liveness containment, not a terminal-action security sandbox.

Outcome: `probe_succeeded`, `command_ready=false`, `read_only=!initialize`,
progress report, optional textual error, and required `response_limit` (null
normally). A nonnull response-limit record has `omitted_fields`, nonnegative
`captures_dropped`/`inputs_dropped`, and boolean `serialization_failed`. Reject
unknown/missing outcome and limit-record fields.
Supervisor rejects command-ready true or more than 128 captures; it does not
independently validate every progress field.

Progress fields: candidate, opened, descriptor-verified, phase, initialized,
output reports/bytes, patterns-written, captures, capture-truncated, elapsed.
Unlike CT, AKP03 diagnostic output retains raw reports and may include native
error text. Treat it as sensitive diagnostic data rather than public telemetry.

A zero-duration passive native probe still enumerates, opens/rechecks identity
and verifies descriptor, but reads/writes no reports. Missing feature/platform
fails native probing; health check works without either.

### Explicit CLI integration and current platform limits

`akp03-probe` accepts only `--capture-ms` followed by unsigned ASCII decimal
`0..30000` (default `2000`) and valueless `--initialize`, `--test-pattern`, and
`--health-check` flags. Unknown, repeated, missing or malformed options, test
without initialize, and health/write conflicts fail before worker launch.

The named private `__akp03-worker` route reuses the same-executable
bounded-stdio service (Windows Job or macOS ARM64 process group).
It receives a complete framed options object under kind `akp03_hid_probe`,
rather than unframed command-line options. Its static allowlist authorizes only
AKP03; no health, CT, Micro, arbitrary command, path or selector is admitted.
The shared `__probe-worker` diagnostic registry also recognizes the AKP03
callback. Both routes stay hidden from help and reject extra arguments.
Duplicate frame keys and unknown/missing typed option/result fields fail
closed. Live callbacks require detached, one-process kill-on-close Windows Job
containment or the macOS private process-group worker capability before backend
construction. Only the child can enumerate/open HID.
The outcome also includes `hardware_qualified=false`; neither readiness nor
qualification can be asserted by a worker. Read-only must equal `!initialize`,
and at most 128 captures are admitted, without independently validating every
progress field. Failed reports go to stderr with nonzero CLI status.

Health uses no backend/transport and requires no feature. Where there is no
Windows/macOS ARM64 supervisor, only this pure no-I/O report runs in-process; live access
has no fallback. Native probes require `akp03-hid`. Windows can read optional
exact BusTypeGuid metadata through a bounded native parent walk across logical
HID buses (missing/unknown transport stops the walk; only the documented USB
bus GUID is classified). Incomplete scans or unresolved matching candidates
fail closed, not false-unique.
The required native interface number/raw descriptor remain unavailable,
so it fails closed before transport open/reports. The raw-descriptor IOCTL
is HID minidriver traffic, not a supported user-mode collection request.
Windows HID access requires a private scoped contained-child capability and
post-open identity/bus revalidation; see platform appendix §2.5.
macOS ARM64 now supports the exact route through the bounded
process-group child. All IOKit access, including synchronous output, requires
the private scoped capability and same-executable parent; public in-process
native access fails closed. Native properties/exact descriptors remain required
and unverified on hardware. A 50 ms independent child parent-lifetime watchdog
can terminate a blocked main thread on parent death, but does not provide
kernel-enforced parent-death/escape containment or overcome process suspension/
OS-driver stalls. See platform appendix §8.7.
Windows failure cleanup may additionally wait up to five seconds for exit and
one for I/O joining; macOS group cleanup allows two seconds per attempt (Drop
may retry once) and closes pipes
synchronously. Unconfirmed cleanup fences replacement.

Result budgeting occurs only after native acquisition/output finishes. Compact
payloads are limited to 65280 bytes, leaving 256 bytes inside the unchanged
65536-byte frame for envelope/identity. Oversize results become explicit failures
with `response_limit`, never EOF or silent success. Rebuild with another 1024
bytes reserved for limit metadata and retain a prefix of complete captures;
never clip raw hex or decoded records. Set `capture_truncated`, record the exact
`captures_dropped` count, and keep initialization/output/pattern accounting.
Native primary errors up to 4096 bytes remain, followed by the response-limit
error; oversized text is explicitly listed as omitted. If needed, a compact
typed failure omits candidate/phase/error text with metadata while retaining all
numeric progress and initialized flags. `inputs_dropped` is always zero for
AKP03. A response-limit record cannot claim probe success.

The shared encoder has a correlated `response_encoding_failed` fallback on
serialization/frame errors, mapped to `frame_limit` after supervised exit.
No Windows runtime or physical input/display acceptance is
claimed. Initialized output may leave brightness/display changed after release.

## 10. Offline control-map export

This is documentation data, not a runtime action controller.

* Arguments: required output directory, optional existing UTF-8 documentation
  file; missing directory argument or a third positional argument fails.
  There is **no default output directory** or implicit documentation target.
* Works without the Windows raster backend and produces no images by itself.
  It does not need fonts, display hardware, HID support or serial access.
* Create the output directory recursively, write/overwrite `control-map.js`,
  then print `wrote <path>` on stdout. If documentation argument is present,
  read it, regenerate marked sections, overwrite it, then print `updated <path>`.
  Propagate filesystem/UTF-8/marker errors; operations are not transactional.
  A documentation failure can leave successfully regenerated JavaScript in place.
* Deterministic LF JavaScript assignment `window.CT_CONTROL_MAP`, schema version
  1, includes 42-control inventory, evidence legend, proposed roles/bindings,
  observed payloads, command reference and nine unresolved questions.
* Emit ASCII only: escape non-ASCII as UTF-16 `\uXXXX`, surrogate pairs included.
  A replacement should use neutral provenance, not another project's identity.
* Records include stable name, optional raw ID, input/address description,
  kind, position group/row/column, labels/legend, capabilities, evidence, proposed
  role/bindings, parity references and question IDs. Role is first binding's role
  or unassigned. Deduplicate parity references in first-seen order.
* Generate Markdown inventory/question tables; escape vertical bars and use
  Unassigned/em dash where appropriate.
* Example accepts output directory and optional documentation path, creates
  directory/writes `control-map.js`; reject a third positional argument.
* Optional documentation update normalizes CRLF to LF and replaces the two
  generated control-table/open-question sections only. Require matching ordered
  BEGIN/END markers; missing markers fail, never guessed insertion.
  Exact marker pairs are `<!-- BEGIN GENERATED ct-open-questions -->` /
  `<!-- END GENERATED ct-open-questions -->` and
  `<!-- BEGIN GENERATED ct-control-table -->` /
  `<!-- END GENERATED ct-control-table -->`. Replace the first matched section
  for each ID, inserting two newlines after BEGIN and one before END.
  Text outside those sections is preserved apart from CRLF normalization.
* The only normal write effects are that JavaScript file and the explicitly
  supplied documentation file. No viewer HTML, SVG, PNG, build artifact,
  configuration or network request is generated implicitly.

Open proposal questions are: Enter also Send; dictation location; strip roles;
refusal haptics; roles for home/Undo/Save/Fn/E/rounds 7–8; other knobs/presses/
wheel touch; switcher cancel control; A/B navigation-versus-command letter clash;
Health touch details. These remain decisions, not hidden defaults.

## 11. Deterministic acceptance vectors

Automated acceptance must inject transports/rasterizers. No test should enumerate,
open or write live devices. Physical qualification is a separate supervised gate.

### CT decoder and pixels

| Wire bytes | Expected |
|---|---|
| `82 05 05 00 00 07 00` | Transaction 0, round_1 down |
| `82 05 05 00 00 07 01` | Transaction 0, round_1 up |
| `82 04 04 00 00 ba` | Tick `ba`, not button |
| `82 05 05 01 2a 00 ff` | Transaction 42, wheel delta −1 |
| `82 09 09 4d 00 00 00 73 00 e2 13` | Standard contact ID19, (115,226), key_08 |
| `82 09 09 6d 00 00 01 bf 00 4c 12` | Standard end ID18, (447,76), right strip |
| `82 09 09 52 00 00 00 96 00 8e 00` | CT contact ID0, (150,142), wheel screen |

Changing last touch opcode 52→72 means end. Delta 80→−128, 7f→127, 00→0 raw;
only ±1 passes profile conversion.

Additional decoder assertions:

* Standard ID1 `(60,0)`→key_00, `(419,269)`→key_11, `(420,0)`→right strip.
* `(480,0)`, `(100,270)`, standard ID0, CT ID5, CT ID0 `(240,0)` name nothing.
* Split streams at every byte boundary, coalesce multiple frames, verify explicit
  suffix handling. Exercise payload length 255 with embedded 82.
* Wrong initial 88, lengths 0/1/2, configured maximum+1 persistently fault.
  Every partial EOF fails. Reset never joins old/new generation.
* Inner-length mismatch, state 2, button ID1b, rotate ID7 and input opcode07 fail.

Six row-major RGB pixels red, green, blue, white, black, `(ab,cd,ef)`, geometry 3×2:

```text
Center/left/right: 00 f8 e0 07 1f 00 ff ff 00 00 7d ae
Wheel:            f8 00 07 e0 00 1f ff ff 00 00 ae 7d
```

`RGB(7,3,7)→0000`, `(8,4,8)→0821`, `(ab,cd,ef)→ae7d`.
Full red center: 194,400 bytes, every pair `00 f8`.
Reject zero/oversized dimensions and missing/extra pixels.

Key11 is `(270,180,90,90)`, 16,200 bytes. Side buffers 32,400 bytes each; wheel
115,200. Preserve caller generation. Alpha 0 leaves background, 255 paints
foreground, 128 gives rounded blend. Bad coverage fails.

### CT presentation/text

* All geometry limits fit bounds; all wheel text corners remain inside safe ring.
* Confirmed slot1/pending5: key0 SELECTED solid white, key8 PENDING dashed cyan,
  wheel PENDING5. Stale preserves confirmed1, clears pending/feedback.
* Neutral view has six dim numbers only. Unfocused status changes its key only;
  focused label changes key+wheel; selection transition changes markers+wheel.
* Every state has a distinct word; unavailable states share color family but
  different words; refusal reason words remain distinct.
* Whitespace `"  a<TAB><LF><LINE-SEPARATOR> b  "`→`"a b"`.
* U+202e + `evil` + U+2066 + `x` + U+0007 + `y`→`evilx y`.
* U+e000 and U+0378→two U+fffd. Attached family-emoji joiners remain attached.
* `abcdef` cap4→`abc…`; `ab cd` cap4→`ab…`; three accented-e clusters cap2→one
  accented-e plus ellipsis. Ten-thousand x→39 x plus ellipsis.
* `a` plus 50 acute combining marks→a plus 4, substituted true.
* First Hebrew/Arabic letter→RTL; digits then Hebrew→RTL; Latin then Arabic→LTR;
  digits only→LTR.
* Repeated fresh/native raster instances match on a fixed machine; text/icon
  pixels cannot escape requested coverage rectangle or wheel mask.

### CT probe

Synthetic successful response:

```text
HTTP/1.1 101 Switching Protocols\nUpgrade: websocket\nConnection: Upgrade\n\n
82 0e 0e 03 01 [ASCII TEST-DEVICE]
82 0b 0c 07 02 00 02 08 00 09 00 00 00
```

Expected identity length 11, firmware `[0,2,8]`, one reply-length disagreement,
no identifier in serialized report. Exercise every split, byte-at-a-time CRLF
and 2-byte partial writes. Written concatenation equals exact upgrade request,
identity/firmware vectors and one close.

Further scenarios:

* Interleaved input captured/tick counted; wrong opcode/transaction or duplicate
  identity while waiting firmware fails.
* HTTP200, missing/duplicate Upgrade, invalid Connection, NUL or 1,025-byte header
  fail, no queries/close. Incomplete header times out.
* First 82/88→suspected stale, only upgrade request written.
* Pattern reply payloads `04 10 03`, `04 0f 04`, `04 10 05`, `04 0f 06` accepted
  as three-byte replies with declared-length disagreements; visual still
  unqualified.
* Event limit1 during capture→one sample and input-limit completion. Hundreds of
  ticks with byte limit256→byte-limit failure.
* Cancel before start→zero writes. Malformed input, line error, read error,
  disconnect or truncated frame fails with retained privacy-safe progress.
* Valid upgrade then identity timeout may close once. Partial/failed query,
  cancel/disconnect or pending cleanup must not close.
* Close failure records write-failed without replacing primary outcome.
* Cancel racing completion at45 ms→cancelled, retain storage through completion.
  Permanently pending deadline20 ms→cleanup-pending around520 ms, retain handles.
* macOS synthetic discovery rejects wrong USB identity, missing/multiple serial
  endpoints and changed registry identity. Worker-scope tests reject direct,
  health-only and stale CT authority. Synthetic native-I/O tests cover idle
  reads, data transfer, hangup and polling cancellation without real hardware;
  a real pseudo-terminal exercises tty idle/data/EOF, deadlines and cancellation;
  shared protocol tests cover partial writes and interrupted sessions.
* Seeded native preparation phases keep their own start times, even with a
  zero-duration capture. Fast success and failure reports remain valid at the
  worker boundary; preparation time is not counted twice.

### AKP03 decoding and image packets

| Hex report | Profile/result |
|---|---|
| `41434b00000000000001` | Original button0 press observation |
| `41434B00000000000091` | Original encoder0 +1 |
| `41434b00004f4b00000101` | Rev2 button0 pressed |
| `41434b00004f4b00000100` | Rev2 button0 released |
| `41434b00004f4b00003501` | Rev2 encoder1 pressed |
| `41434b00004f4b00003300` | Rev2 encoder0 released |
| `41434b00004f4b00003401` | Rev2 encoder2 pressed |
| `41434b00004f4b00000000` | Rev2 acknowledgment |
| `41434b00004f4b0000ff00` | Rev2 unknown-control error |

Pad 11-byte vectors with 501 zero bytes to 512: same results. Original decoding
of `41434b00004f4b00000101` is acknowledgment, not a push.
ACK/OK code pairs 51/50→encoder1 +/−1; 91/90→encoder0; 61/60→encoder2.
Plain ACK code0 without OK→no-control.

Reject original lengths 0/1/3/9/513/1024, revision2 length10, wrong prefix,
codes07/24/32/ff, push-state02, odd/nonhex/nonASCII or >1,024-character hex.

Raw boundary-only images of lengths 4/1023/1024/1025/65535 yield report counts
3/3/3/4/66, exact announced length and zero tail. Reject size0/3/65536, bad markers
and keys6/8/255.

For raw marker bytes `ff d8 ff d9`, key0:

```text
BAT prefix: 00 43 52 54 00 00 42 41 54 00 00 00 04 01
Raw prefix: 00 ff d8 ff d9 [zeros]
STP prefix: 00 43 52 54 00 00 53 54 50 [zeros]
```

Rotation test: red source top-left→top-right, green top-right→bottom-right,
blue bottom-left→top-left. JPEG frame dimensions64×64; original unchanged60×60.
Wrong profile-size RGB fails.

Exact descriptor accepted; changed byte48, empty or keyboard `05 01 09 06`
rejected. Related products1002/3003 or usages1:6/ffa0:2 do not inherit support.

### AKP03 probe

* Passive 6.5-second capture writes zero reports.
* Short initial write10→stop immediately, no reads, initialized false,
  patterns0, successful_reports0, output_bytes10.
* Initialized pattern capture0: DIS,LIG50,CONNECT,CLE,STP, then six BAT image
  transactions with one-based keys1..6; patterns6; no LOG command.
* Unknown 512-byte input every1 ms→128 retained entries, each1,024 hex characters,
  input absent/decode-error present, truncation true.
* Health check constructs no transport.
* Zero-duration passive native probe verifies interface/descriptor without
  reports. Success never enables command readiness.

## 12. Integration dependencies and unresolved gates

Connect these helpers to the package's shared contracts rather than duplicate
authority logic:

* Slot 1–6, statuses, actions/refusals, binding health and semantic token styles.
* Consent/title selection, freshness, availability, refusal lifetime and
  confirmed/pending selection.
* Input control kinds/intents, event kinds, connection generation and shared
  clock samples. Raw decoder local time is not an action timestamp.
* Realized-slot geometry, marker/effect semantics and independently drawn artwork.
* CT discovery facts/COM normalization; optional HID backend.
* Same-executable worker dispatch, bounded stdio serialization, watchdog and
  kill/reap. Stuck CT requests require helper exit, not in-process reconnect.
* Firmware capability reporting: explicitly unavailable for both.

Open gates remain: CT proposal versus actual recommendations; safe CT touch
recognition; complete CT physical mapping/orientation; Windows HID full-write
return conventions; usable original-profile push semantics without fabricated
releases; AKP03 rollover/holds/debounce/reconnect/sustained operation.
Do not turn unqualified pure helpers into an implicitly authorized live worker.
