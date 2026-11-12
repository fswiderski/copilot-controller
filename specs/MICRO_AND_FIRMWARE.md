# Micro device and firmware behavior

This appendix specifies independent functional parity for the Micro device family in `copilot-controller`. It specifies externally observable behavior and safety boundaries, not a source-code layout. Keep the public CLI family neutral: `micro-*`. Device I/O, diagnostics, session-command authorization, semantic presentation, and firmware maintenance are separate capabilities.

The deterministic scenarios below use synthetic transports and injected clocks. They do **not** establish physical qualification. No descriptor assets, implementation code, or firmware binaries are distributed by this specification.

## 1. Capability and safety boundaries

- Ordinary vendor RPC permits four methods only; bootloader entry is unavailable through it.
- Passive diagnostics send zero HID reports. Query operations send reports but do not write configuration.
- Successful diagnostics and firmware updates always return `command_ready=false` and `hardware_qualified=false`.
- Health readiness means a current connected snapshot with loaded configuration, not physical accuracy or permission to dispatch commands.
- Semantic LED output requires matching scoped physical-effect qualification. The explicitly requested diagnostic LED pattern is a separate, unqualified path.
- Input declarations do not imply all declared controls are normalized. In particular, encoder press is declared but not emitted by the strict adapter.
- Firmware input artifacts are **local, explicitly supplied files** with exact sizes and digests. Downloads, arbitrary substituted tools, implicit upgrades, and automatic recovery are not part of this contract.

## 2. Device identity and ownership

### 2.1 Constants and timing

| Property | Required value |
|---|---|
| Application VID | `0x303A` |
| Application PID | `0x8298` |
| Vendor usage page | `0xFF00` |
| Vendor collection usage | `1` |
| Vendor report size, including ID | `64` bytes |
| Windows reference interface | `MI_02` |
| HID device-interface class | `{4D1E55B2-F16F-11CF-88CB-001111000030}` |
| Windows interface prefix | `\\?\HID#VID_303A&PID_8298&MI_02#` |
| Compatibility ownership mutex | `Local\CreatorMicroDevice-303A-8298` |
| Competing image names | `chatgpt.exe`, `input.exe` |
| Reference write timeout | `3s` |
| Reference read timeout | `100ms` |
| Reconnect delay | `3s` |
| Device-status interval | `10s` |
| Conflicting-process scan | `5s` |
| Status-snapshot scan | `750ms` |

Synchronization names above are compatibility identifiers. The pure timing/owner rules do not themselves implement a device loop. The bounded native probe uses reads of at most `50ms` and has no separate enforced HID-write deadline.

### 2.2 Textual Windows enumeration

Decode enumeration bytes lossily as UTF-8. Find every non-overlapping occurrence of the interface prefix followed by at least one non-whitespace character. A match may start inside other text. Its suffix ends at Python-style whitespace: Unicode whitespace plus `U+001C..U+001F`.

Match ASCII prefix letters case-insensitively; `I` also matches `U+0130` and `U+0131`. Preserve original spelling and replacement characters. Exactly one match is required; duplicate matching paths count twice. A bare prefix followed by whitespace does not match. Other interface numbers do not match this textual selector.

Validate capabilities independently:

- Capability-query status must be `0x00110000`.
- Usage page/usage must be `0xFF00/1`.
- Input and output report lengths must each be `64`.
- Failed capability query and unexpected layout are different errors.
- Report validation accepts exactly `64` bytes.
- Write completion accepts exactly count `64`, not merely a positive count.

For conflict scanning, extract the final Windows path component, treating `/` and `\` as separators, ignoring empty/dot components, recognizing drive-relative paths such as `C:input.exe`, and removing UNC server/share anchors. Lowercase, filter to the two exact conflicting image names, sort, and deduplicate. A name ending in `.old` is not a match. Any remaining name blocks ownership.

These are pure selection/ownership policies. Do not claim that Windows native diagnostics actually invoke the mutex/process scanner unless the surrounding driver integrates them.

### 2.3 Native discovery and qualified opening

Native discovery requires `micro-hid` on Windows or macOS. Enumerate all collections with the application VID/PID, including keyboard/mouse collections, and expose:

| Candidate field | Type |
|---|---|
| `vendor_id`, `product_id`, `usage_page`, `usage` | unsigned 16-bit integers |
| `interface_number` | signed 32-bit integer |
| `transport` | `"usb"`, `"bluetooth"`, or `"unknown"` |

Use the native bus classification, not a name/path heuristic.

Opening filters to the exact VID/PID/page/usage tuple and an optional required transport, then requires **exactly one** candidate. Do not first discard unknown descriptors and pick one of several remaining devices.

Open the enumerated exact device path. On macOS use nonexclusive opening, preserving keyboard/mouse operation. Re-read opened identity and require VID, PID, interface number, bus classification, and path to match. Do not require reopened usage to equal selected usage: macOS may expose the keyboard primary usage for a composite path.

Read at most `1024` descriptor bytes. Before sending any report, match exact descriptor bytes against these qualified profiles. Comparing actual descriptor length and SHA-256 provides the required byte-exact qualification without shipping descriptor assets:

| Bus/interface | Length | SHA-256 |
|---|---:|---|
| USB/interface `0` | 275 | `9257d7361f9c784e0fc0b260bbac0feadd49bf79cbb6202d6c41560cbae96fb6` |
| USB/interface `2` | 36 | `08229e86dff4b74981ad1c14f06deaee61a6d41d147273f2cdde860ea435986b` |
| Bluetooth/interface `-1`, macOS only | 206 | `a173b5298035f16f1d96a99d8cef32ae8176262da4666ee18ad74f9aa3a23115` |

The USB interface-0 profile has keyboard report `1`, consumer `2`, mouse `3`, gamepad `4`, and vendor `6`. The USB interface-2 profile has vendor report `6` only. The Bluetooth profile has keyboard `1`, consumer `2`, mouse `3`, and vendor `6`, without gamepad `4`.

Vendor-only descriptor item semantics, in order:

1. Usage page `0xFF00`; usage `1`; application collection.
2. Report ID `6`.
3. Usage `2`; logical minimum `0`; logical maximum `255`; size `8`; count `63`; input Data/Variable/Absolute.
4. Usage `3`; logical minimum `0`; logical maximum `255`; size `8`; count `63`; output Data/Variable/Absolute.
5. End collection.

Encoding widths are part of exact qualification: usage page and logical maxima use two data bytes; other numeric items use one. Collection type is `0x01`; input/output flags are `0x02`.

The USB interface-0 measurement corresponds to firmware `v0.1.50`; interface `2` to `0.6.3`. The Bluetooth descriptor was measured on a paired `0.6.3` device. These observations are not blanket physical qualification. Reject any changed descriptor byte, wrong bus/interface/identity, keyboard-only descriptor, or unknown transport before sending reports. Bluetooth descriptor qualification is not enabled on Windows.

### 2.4 macOS local lease

Diagnostics and firmware maintenance share a nonblocking, per-effective-user file lock:

- Canonicalize the OS temporary root.
- Use private directory `copilot-controller-<uid>` and file `device.lock`.
- Create directory mode `0700` when absent; require directory type, effective-UID ownership, and no group/other permission bits.
- Open/create lock read/write without truncation, mode `0600`, refusing final-path symlinks.
- Require a regular file owned by the effective UID with no group/other bits.
- Acquire an exclusive nonblocking lock and retain the handle throughout the operation.

Failure means another participating controller operation owns the device or the lease is unsafe. This lock does not exclude unrelated software. Windows native diagnostics do not invoke this lease.

## 3. HID wire framing

### 3.1 Output

Serialize compact UTF-8 JSON with no BOM, spaces added by formatting, terminator, or newline. Split bytes into chunks of at most `61`; a UTF-8 code point may span reports.

| Byte range | Meaning |
|---|---|
| `0` | report ID `6` |
| `1` | JSON channel `2` |
| `2` | chunk byte count |
| `3 .. 3+count` | JSON bytes |
| remainder through `63` | zero padding |

Every output report is exactly `64` bytes. There is no sequence number, checksum, start flag, final flag, or per-fragment acknowledgement. Successful full JSON parsing identifies message completion. The encoder has no independent one-megabyte limit.

RPC request field order is `method`, `params`, `id`. The initial version request is exactly:

`{"method":"sys.version","params":null,"id":101}`

It is `47` bytes: header `06 02 2f`, those `47` ASCII bytes, then `14` zeros.

A filesystem request example is:

`{"method":"fs.read","params":{"file":"keymap.json"},"id":102}`

Generic nested JSON maps use sorted keys in the parity behavior. Preserve explicitly specified request and LED-entry field ordering.

### 3.2 Decoder

Low-level framing accepts packets shorter than `64` provided the header and declared payload are present. Native I/O and offline replay add the exact-64-byte boundary.

Reject as invalid frame when:

- Packet has fewer than three bytes.
- Report ID is not `6`.
- Declared count exceeds `61`.
- Declared payload is longer than available bytes.

Validate this before channel handling:

- Channel `1`: log, ignore without disturbing JSON accumulation.
- Channel `2`: JSON.
- Any other channel: unknown-channel error.

Zero-length JSON chunks do nothing. When the buffer is empty, a nonempty chunk consisting entirely of CR/LF does nothing and does not start message age. Other whitespace begins accumulation.

On the first accumulated bytes, record a monotonic start timestamp. Append JSON payloads; reject after appending if buffer size is greater than `1,048,576` or age is greater than `5s`. Exactly `5s` is permitted. Saturate backward-clock elapsed time to zero. Check age only on nonempty appended JSON chunks, not on reads without data or ignored reports.

Parse the **entire** buffer after every append:

- Any parse failure retains bytes, even clearly malformed JSON, invalid/incomplete UTF-8, `NaN`, or concatenated JSON values.
- There is no delimiter resynchronization or JSON-stream splitting.
- Successful parsing clears buffer and start time.
- Yield only JSON objects; a successfully parsed nonobject fails after clearing.
- Oversize/overage errors retain the buffer; caller replaces the decoder on reconnect.

## 4. Ordinary RPC and errors

### 4.1 Admission, IDs, response matching

Allow only exact case-sensitive methods:

- `sys.version`
- `device.status`
- `fs.read`
- `v.oai.thstatus`

For `fs.read`, parameters must equal exactly `{"file":"keymap.json"}`, with no extra fields. Reject file writes/deletes, other reads, self-test, layer writes, and bootloader entry.

Fresh request IDs start at `101` and increment once per admitted call. Rejected methods/parameters do not consume IDs. The unsigned 64-bit counter has no specified wraparound strategy.

Write every request report first. Start a four-second response deadline after the last write. Check `now < deadline` before each read; equality fails. The response-feeding function does not independently check time, so a response from a read started before the deadline can complete at its endpoint. Empty reads are no data, not framing failures.

A response completes a call only when its `id` is numerically equal to the pending integer and it contains `result` or `error`.

- Ordinary matching uses Python-like JSON numeric equality: `101.0` equals `101`; `"101"` does not. Booleans compare as `0/1`.
- An echoed `method` is neither required nor checked.
- A matching `error` field wins over `result`, even if `error` is null.
- A `result` may be any JSON type including null.
- Matching ID without `result`/`error` is not completion.

For unmatched objects, queue only notifications with `m` equal to `v.oai.hid`, `v.oai.rad`, or `kb.radial`. Drop other unsolicited messages. Preserve FIFO order. Capacity is `128`; the 129th admitted notification fails without enqueueing or discarding previous items.

### 4.2 Ordinary firmware gate

Require `sys.version` result to be an object. Missing/falsy `version` means unsupported firmware. A truthy nonstring version or nonobject result means malformed result.

Remove all leading lowercase `v` characters. Parse only the first two dot-separated components as base-10 signed integers, ignoring later components:

- Allow surrounding Python whitespace, optional sign, ASCII digits, and single underscores between digits.
- Reject non-ASCII decimal digits, hexadecimal, leading/trailing/repeated underscores, suffix text in either parsed component, and whitespace after sign.
- Saturate enormous values while preserving comparison ordering against `0/6`.
- With two components require `(major,minor) >= (0,6)` lexicographically.
- With one component require `major > 0`.
- Return original spelling unchanged.

Accepted: `0.6.3`, `0.6`, `v1`, `1`, `vv0.10.0`, `" 0.6"`, `2.0.x-rc`.

Rejected: `0.5.9`, `0`, `v0`, `x.6`, `v.0.6`, `0.6-beta`, `2.0-rc`.

This is not semantic-version parsing. The maintenance path below deliberately uses a different, exact-version admission rule.

### 4.3 Error lifecycle and rendering

Keep separate errors for framing, unknown channel, incomplete/oversized message, nonobject message, method rejection, restricted filesystem read, device rejection, response timeout, input overflow, unsupported/invalid firmware version, malformed firmware result, report size, short write, capability failure/layout, interface count, competing owners, invalid telemetry, malformed event, and serialization failure.

The reference lifecycle disposition is:

- Malformed event, malformed firmware result, or serialization failure: stop controller; attempt normal shutdown cleanup and record stopped state.
- All other errors above: record `last_error`, close device, retry after `3s`.

Pure protocol helpers return disposition; they do not execute the lifecycle. A decoding error abandons the current event and remaining pending queue via reconnect/stop.

Stable relevant diagnostics:

| Condition | Text |
|---|---|
| Frame | `Invalid HID vendor frame` |
| Channel | `Unknown HID vendor channel` |
| Accumulation limit | `Oversized or incomplete vendor JSON message` |
| Top-level type | `Vendor RPC message must be an object` |
| Method | `Device method is not allowlisted` |
| Filesystem | `Only reading keymap.json is supported` |
| Device rejection | `Device rejected <method>: <rendered-error>` |
| Deadline | `No device response to <method>` |
| Queue | `Device input queue overflow; reconnect required` |
| Version gate | `Unsupported firmware: <rendered-version>` |
| Version parse | `invalid literal for int() with base 10: <quoted-component>` |
| Result shape | `Malformed sys.version result` |
| Report size | `HID reports must contain exactly 64 bytes` |
| Write count | `Short HID write: <count>` |
| Caps query | `HidP_GetCaps failed` |
| Caps layout | `Unexpected Micro vendor HID report layout` |
| Telemetry | `Invalid joystick telemetry` |
| Event shape | `Malformed device event` |

Owner/interface diagnostics include sorted conflicting names or actual match counts.

For diagnostic rendering, null becomes `None`, booleans `True/False`, and strings render directly under string conversion but quoted when nested. Arrays/objects use comma-space separators and object keys are sorted. String representation chooses double quotes only when an apostrophe exists and no double quote exists; otherwise single quotes. Escape backslash, selected quote, tab/newline/CR, controls, DEL, and Unicode nonprintables using Python-style hexadecimal escapes. Preserve printable Unicode.

Floating representation uses shortest round-trip digits, positional notation for exponents `-4..15`, scientific notation outside it, explicit exponent sign, and at least two exponent digits. Preserve integral-float versus integer rendering, including `-0.0`.

## 5. Legacy input decoding and calibration

### 5.1 Event forms and state

HID input shape:

`{"m":"v.oai.hid","p":{"k":"<key>","act":<action>}}`

Radial shape:

`{"m":"v.oai.rad","p":{"a":<angle>,"d":<distance>}}`

The radial alias is `kb.radial`.

Missing `p` behaves like an empty object. Present nonobject `p`, including null, is malformed. Legacy `act` comparison uses Python-like numeric equality, accepting `true`/`1.0` as `1` and `false`/`0.0` as `0`. Unknown/numeric keys normally do nothing; array/object `k` is malformed in the command-button parser.

All held/debounce/telemetry state is per connection. Monotonic time drives debounce; wall-clock Unix seconds drive telemetry/calibration timestamps.

### 5.2 Slot keys and dial

Exact uppercase `AG00..AG05` select one-based slots `1..6`:

- `act=1`: first down transition may request selection.
- `act=0`: release, remove held state, no request.
- Other action: ignore.
- Repeated down while held never repeats.
- Per-raw-key debounce is `300ms` since last accepted action; exact boundary permitted.
- Set held state before checking debounce, so a rejected bounced press remains held until release.
- Rejected debounce does not advance accepted time; backward time is inside the window.

`ENC_CW`/`ENC_CC` with `act=2` step forward/backward through the caller's ordered active-slot list, wrapping. Missing active list means all six; explicitly empty means no action. With no/unlisted selection, CW chooses first and CC last. Directions debounce independently by raw key name and use no held-state latch.

Direct AG keys may request inactive slots. Ordinary active dial choices are non-Off LED slots. `ENC_CLK`, `AG06`, lowercase, and suffix whitespace do not select.

### 5.3 Commands and settings

| Key | Label | Default action |
|---|---|---|
| `ACT06` | Run | `autopilot` |
| `ACT07` | A | `accept` |
| `ACT08` | B | `stop` |
| `ACT09` | Branch | `fork` |
| `ACT12` | Send | `send` |
| `DPAD_UP` | D-pad Up | `up` |
| `DPAD_RIGHT` | D-pad Right | `right` |
| `DPAD_DOWN` | D-pad Down | `down` |
| `DPAD_LEFT` | D-pad Left | `left` |

Command buttons use press/release `1/0`, once per hold, with `500ms` per-key debounce from last accepted press. Set held state before debounce. Only release rearms. `ACT10/ACT11` are not command buttons.

Recognizing a key does not authorize dispatch:

1. Consult validated shared bindings.
2. Missing binding or action `none`: unbound, regardless of controls switch.
3. Other action with command controls disabled: controls-disabled, no dispatch.
4. Otherwise return configured action, not necessarily default.

Shared configuration must reject arbitrary command strings and reserved agent keys. Required examples: `AG00=send` and `DPAD_UP=accept` invalid; `DPAD_UP=left` and `ACT07=model_picker` valid. Default settings have all nine default bindings, command controls/voice/automatic binding disabled, and no saved D-pad angles. Recommended settings enable command control and use brightness `0.75`. Shared configuration owns its complete schema/persistence.

### 5.4 Wide Mic coalescing

`ACT10` and `ACT11` are two switches under one wide key. Maintain a down bit per switch:

- Release clears that switch only.
- Press marks its switch down.
- Emit one Mic activation only when neither switch was previously down and at least `400ms` elapsed since the last accepted activation.
- Duplicate down, overlap with the other switch, or early bounce does not emit.
- Both switches must have been released to start a new physical gesture.
- Rejected bounce still marks down; other controls never activate Mic.

Output is only a boolean activation. Voice start/stop, microphone access, authorization, and session integration are external.

### 5.5 Radial normalization and stick hysteresis

For radial methods require finite JSON numeric `a` and `d`, excluding booleans/strings, each in inclusive `[0,1]`. Missing/invalid telemetry is a reconnect-class error. Normalize angle modulo one: `1` becomes `0`.

Every valid sample updates `{angle,distance,timestamp}`, using normalized angle and supplied wall-clock time, even without calibration/action.

Initially armed:

| Distance | Result |
|---|---|
| `<0.25` | rearm; no key |
| `0.25 <= d <0.65` | neither rearm nor emit |
| `>=0.65` | emit only if armed and all four angles exist; then disarm |

Incomplete calibration does not disarm. Deflection does not repeat until returning below `0.25`. Exactly `0.25` does not rearm; exactly `0.65` qualifies.

Select nearest calibrated direction using
`abs(((a-b+0.5) modulo 1)-0.5)` with nonnegative Python-style floating modulo. Tie order is up, right, down, left; replace best only for strictly smaller distance. Return corresponding `DPAD_*` command key. Stick output does not add command-button debounce.

Combined decoding order:

1. Command: stick for radial methods, otherwise command button.
2. Mic.
3. AG/dial slot request.

Radial messages containing `k/act` may therefore also produce Mic/slot output. Legacy AG/Mic/button helpers themselves do not check `m`; routing admits notifications.

### 5.6 Calibration format

`dpad_angles` is an object containing only `up`, `right`, `down`, `left`. Empty/partial objects are valid. Present angles must be finite JSON numbers excluding booleans, in `[0,1)`.

Only when complete, compare all six unordered pairs with the exact floating circular-distance formula. Reject any distance `<0.15`; there is no epsilon. For example `0.94` versus `0.09` rounds below `0.15` and fails a complete calibration. Partial duplicate angles are valid until completion.

Direction/capture/tie order is up, right, down, left. Exported map keys are canonical names in sorted JSON order.

Capture a direction from a status object:

1. Normally `status.joystick={angle,distance,timestamp}`.
2. Missing/falsy joystick acts as an empty object; truthy nonobject is malformed.
3. Missing timestamp/distance numerically default to `0`. For these two arithmetic fields only, booleans count as `0/1`; other nonnumbers are malformed.
4. If `wall_now-timestamp >2`, fail not-held **before** checking distance.
5. If distance `<0.65`, fail not-held.
6. Require angle; merge requested direction into prior angles; validate whole map.

Exactly two seconds old and distance `0.65` pass. Future timestamps pass capture, unlike health. Boolean `distance=true` passes capture's threshold. Capture has no upper-distance check; angle still must be a proper number in `[0,1)`.

Errors:

- `Invalid D-pad calibration keys`
- `D-pad angles must be finite numbers in [0, 1)`
- `D-pad directions overlap; recalibrate while holding each direction`
- `Hold the stick firmly in the requested direction, then capture again`
- `Malformed joystick snapshot`

Authorization answer is exactly `CALIBRATE`, case-sensitive/untrimmed. Quit is trimmed case-insensitive `q`, not `quit`. Interactive orchestration and persistence are external.

## 6. Strict semantic profile

Profile ID: `micro-2-pro`.

Declared recommendations:

| Controls | Kind | Intent |
|---|---|---|
| `AG00..AG05` | Press | select slot `1..6` |
| `encoder_turn` | Turn | move cursor |
| `encoder_press` | Press | commit cursor |
| Five ACT command buttons | Press | default command |
| Four `DPAD_*` | Stick | directional command |

The actual strict event adapter accepts only `m="v.oai.hid"`, object `p`, string `k`, and an unsigned-integer JSON `act`; it does not apply boolean/integral-float coercion:

- `ENC_CW`, `2` → `encoder_turn`, Turn `+1`.
- `ENC_CC`, `2` → `encoder_turn`, Turn `-1`.
- `AG00..AG05`, `ACT06/07/08/09/12`, `1` → named control Press.
- Same keys, `0` → Release.
- Everything else → no event.

Attach supplied connection generation and presentation clock sample unchanged. Malformed input is simply not adapted, rather than surfaced as a protocol error.

**Parity discrepancy:** this adapter does not emit `encoder_press`, Mic, radial telemetry, or `DPAD_*` events despite broader declarations and legacy calibrated decoding. Do not silently add those behaviors as “parity”; additional mappings require separate qualification. Presentation owns debounce/coalescing of strict physical events.

## 7. LED encoding

### 7.1 Legacy status frames

Always repaint six entries, IDs `0..5` in order. Runtime slots are one-based; LED IDs zero-based. Entry fields serialize in order:

| Field | Meaning |
|---|---|
| `id` | slot LED integer |
| `c` | packed `0xRRGGBB` integer |
| `b` | floating brightness |
| `e` | effect ID |
| `s` | floating speed |

Send array as `v.oai.thstatus` params.

| Status/name | Color |
|---|---|
| `off` | `0x000000` |
| `idle` | `0xFFFFFF` |
| `working` | `0x2070FF` |
| `complete` | `0x00D060` |
| `waiting` | `0xFFAA00` |
| `error` | `0xFF3030` |
| `unknown` | `0x607D8B` (`96,125,139`); must not become Off |

This is the shared semantic Unknown RGB, not an independently chosen device color.

Default brightness `0.25`, effect `1`, speed `0.2`. With a selected slot, that slot gets requested brightness and all others half. Without selection all get full brightness. Selection is static contrast, not an assumed native pulse. The low-level legacy constructor itself does not validate brightness.

Example selected slot 1/all idle/requested brightness `0.4`:

`{"id":0,"c":16777215,"b":0.4,"e":1,"s":0.2}`

Other entries use brightness `0.2`.

Shutdown: six black entries, each `b=0.25,e=1,s=0.2`; not zero brightness.

Dial-active slots are all non-Off colors, including Unknown, returned in ascending one-based order.

Diagnostic pattern, IDs `0..5`: idle, working, complete, waiting, error, off. Select slot 1 at base `0.25`, producing brightness `0.25` for ID0 and `0.125` for the other five.

### 7.2 Qualified semantic output

Reject encoding unless device kind is Micro, profile is `micro-2-pro`, ID `<6`, and brightness finite/inclusive `[0,1]`.

For native realization with effect ID/speed require a qualification:

- Feature SlowPulse.
- Same semantic token.
- Capability authorizes exact scope for visible use.
- Same native ID and exact speed.
- Finite speed `>0`.
- Present smooth-cycle period `>=1000ms`.

For all non-native realizations require:

- Feature Steady.
- Same token and valid exact-scope visible-use capability.
- Finite qualified native speed `>=0`.

Use matching qualification's native ID/speed for steady output and matching realization's ID/speed for native pulse. No hardcoded qualified steady ID or speed upper bound exists here. Pack realized RGB and retain realized brightness. Missing qualification returns no entry; never silently use an unqualified fallback.

Native IDs `1/4` and speed `0.2` are physical assumptions, not proof. Diagnostic output uses only effect `1` and static dimming; do not restore an assumed selected-slot pulse.

## 8. Snapshot health and calibration readiness

### 8.1 Input validation

Health never opens HID. Inputs are missing file, unreadable file with error text, or raw bytes. Remove one leading UTF-8 BOM, require UTF-8 and standard JSON. Missing file acts as an empty object and fails heartbeat validation.

Status:

- Object required.
- `heartbeat`: required finite JSON number, not boolean, `>0`.
- `connected`, `stopped`: optional booleans; present null invalid.
- `bindings`: optional object; every row must contain object `health`; `health.live` optional boolean.
- `slots`: optional array of at most six recognized lowercase LED color names, including `unknown`; empty/short arrays allowed.
- `last_error`: optional null/string.
- Additional fields tolerated; `stop_reason` can have any JSON type.

Validation diagnostics in order:

1. `status.json must be an object`
2. `status.json has no valid heartbeat`
3. `status.json connected must be true or false`
4. `status.json stopped must be true or false`
5. `status.json bindings must be an object`
6. `status.json contains invalid binding health`
7. `status.json contains invalid slot states`
8. `status.json last_error must be text`

### 8.2 Result schema and diagnosis

Device-health result:

- `controller_fresh`: boolean.
- `connected`: boolean/null.
- `checks`: array of `{name,level,detail,action}`, all text except level constrained to `"ok"|"warning"|"error"`.

Use wall-clock Unix seconds. Recent heartbeat requires `0 <= now-timestamp <10`. Exactly `10s` old or future timestamps are stale. A validated status is fresh only if not stopped and recent.

Rows: controller, usb, optional dpad, optional last reported error.

- Invalid/unreadable status → controller Error, diagnostic detail, fresh false, connected null.
- Fresh → controller Ok, empty action, `Current; heartbeat age <one-decimal>s.`
- Stale/stopped/future → controller Error, restart guidance, `Stopped, stale or clock changed; heartbeat age <one-decimal>s.`
- If stopped append ` Stop reason: <rendered reason>.`; absent reason is `not recorded`, explicit null renders `None`.

Only fresh status yields connected boolean: exactly `connected=true` becomes true, absent/false becomes false. Historical status always yields null.

| Connected | USB row |
|---|---|
| true | Ok; `Connected in the latest controller snapshot.` |
| false | Warning; `Not connected in the latest snapshot.` |
| null | Warning; `Unknown until the controller is current.` |

USB guidance: data cable, intended application layer, close competing owners, let the single controller retry; health must not open the device.

D-pad calibration known true → Ok, `Four directions saved; physical accuracy is not checked.` Known false → Warning, `Not fully calibrated.` Unknown/not loaded → no row. Guidance requires controller/USB readiness before calibration and enabled command controls for navigation.

For nonempty validated `last_error`, add Warning named `last reported error`:

- Fresh prefix `Latest snapshot: `.
- Historical prefix `Historical snapshot (not current): `.
- Warn against blindly retrying Send/Accept; inspect intended selected/foreground session and actual dialog/draft; consult local log.

Sanitize every detail by replacing Unicode category C characters—controls, format, surrogate, private-use, unassigned—with one space each. This replaces ESC/newline but does not parse the remaining ANSI syntax; ESC`[2J` becomes a space plus `[2J`. Other separator categories are retained.

Readiness with loaded configuration is true iff fresh, connected true, configuration loaded, and no Error row. Warnings, missing D-pad calibration, or last-error text do not independently block this predicate. It is not command authorization or qualification.

### 8.3 Calibration precondition is a separate predicate

Require controller process running; no nonempty `status_error`; object status; exactly connected true; stopped false under PowerShell-style truthiness; signed-integer-or-floating heartbeat; and age in `[0,10)`.

Do not run full status validation. For stopped:

- Null/false/zero/empty string/empty array are false.
- Nonempty string, including `"false"`, is true.
- Singleton array uses member truthiness, except a nested array is true iff nonempty.
- Array of at least two elements is true.
- Object, including empty object, is true.

Thus `[false]` permits, `"false"` blocks, `[[0]]` blocks. Unsigned-only heartbeat values beyond signed-integer range are refused.

## 9. Bounded diagnostics and replay

### 9.1 Options

| Option | Contract |
|---|---|
| `capture_ms` | integer `0..30000`; public CLI default `2000` |
| `query` | firmware query, gate, then status |
| `read_keymap` | requires query; privacy-sensitive keymap result |
| `test_pattern` | requires query; writes pattern then attempts clear |
| `health_check` | conflicts with query/keymap/pattern; no device access |

The programmatic default object has duration `0`, unlike CLI default `2000`. Invalid options produce no writes. A health check cannot be passed to an already-created injected transport runner.

### 9.2 Complete result schema

All structured worker objects below reject unknown fields when decoded. Nonoptional fields remain required even where a programmatic default object exists.

Outcome:

- `probe_succeeded`: boolean.
- `command_ready`, `hardware_qualified`: always false.
- `read_only`: exactly `report.output_attempts == 0`.
- `report`: Progress.
- `error`: string/null.
- `response_limit`: null normally; otherwise an explicit export-budget failure
  record containing `omitted_fields` (fixed field names), `captures_dropped`,
  `inputs_dropped` (nonnegative integers), and `serialization_failed` (boolean).
  This field is required even when null. Unknown/missing record fields fail.

Progress fields:

| Field | Type |
|---|---|
| `candidate` | Candidate/null |
| `opened`, `descriptor_verified` | boolean |
| `phase` | string |
| `firmware` | string/null |
| `firmware_reference_compatible` | boolean |
| `device_status`, `keymap` | arbitrary JSON/null |
| `output_attempts`, `output_reports`, `output_bytes` | nonnegative integers |
| `write_failed` | boolean |
| `read_reports`, `nonvendor_reports_ignored` | nonnegative integers |
| `captures` | Capture array |
| `inputs` | ObservedInput array |
| `capture_truncated`, `led_test_attempted`, `led_clear_acknowledged` | boolean |
| `cleanup_error` | string/null |
| `elapsed_ms` | unsigned 64-bit integer |

Defaults: zero/false, optional values null, arrays empty, phase empty.

Capture: `elapsed_ms` unsigned 64-bit integer, `bytes` nonnegative integer, `hex` lowercase contiguous hex string.

ObservedInput: `elapsed_ms`, `message` object, `slot_requested` one-based slot/null, `command_key_observed` string/null, `mic_observed` boolean, `joystick` `{angle,distance}`/null.

### 9.3 Accounting and budgets

- Read into `65` bytes.
- Before every read require `read_reports <512`; budget exhaustion prevents even a subsequent timeout read.
- Zero-byte result: no data, not counted.
- Result `>64`: reject before incrementing counter.
- Count each nonempty result of at most `64`.
- IDs `1..4`: count ignored nonvendor traffic, return no data; short keyboard reports allowed.
- Other IDs: require length `64`, then ID `6`.
- Capture at most `128` accepted vendor reports, including logs/replies; store elapsed ms, count, lowercase hex.
- Beyond capture capacity mark truncated but continue supplying packets while an RPC needs them.
- Passive capture stops before another read once `128` captures exist and marks truncated.

For writes, increment attempts before I/O, add returned byte count even if short, increment successful report count only after exact `64`. Write error/non-64 count marks `write_failed`; never retry or append cleanup JSON to a potentially incomplete outgoing message.

### 9.4 Sequence and observation

After qualification, create fresh RPC/input state:

1. If query: phase `firmware`, request version/null; preserve observed string; apply ordinary gate; set compatibility. Then phase `status`, request status/null.
2. If keymap: phase `keymap`, read exact keymap file.
3. If pattern: phase `led_test`, set attempted **before** requesting frame.
4. Phase `capture`, acquire for requested duration subject to budgets.
5. If LED test attempted and no write failed: phase `clear_leds`, request shutdown.
6. Finalize `elapsed_ms` and phase `complete`/`failed`.

Before every RPC other than ID101, drain incoming traffic for a fixed `50ms` interval, queue/record inputs using the same decoder, then write. This is not a wait-until-quiet policy. It applies to cleanup. Response reads use slices of at most `50ms` within the four-second deadline.

After each receive/passive read, drain notifications into observations using all six slots active, selected null, empty calibration, and wall time zero:

- AG selection, command names, and Mic are observed, never dispatched.
- Because observed selection is not retained, CW starts at first slot and CC at last on each observation.
- Radial telemetry is observed but no calibrated stick command emitted.
- Latest joystick angle/distance can recur on later nonradial rows.
- Exported joystick omits timestamp.
- Events drained in one call share its supplied elapsed timestamp, not original acquisition times.

At most `128` observed inputs; another fails `Micro input budget exhausted; events not dispatched`. Read-budget exhaustion fails `Micro read budget exhausted`.

Old query firmware remains recorded but fails before status/keymap/LED operations. Passive capture performs no version gate.

### 9.5 Cleanup/failure precedence

Attempt shutdown after an attempted LED test even if its response timed out or later acquisition failed, **unless any write failed**. Reuse existing RPC/framing state, so cleanup may also fail on accumulated parse/budget problems.

Read-budget exhaustion does not prohibit the shutdown write. Cleanup drains
for the fixed 50ms interval while the original 512-report budget permits reads;
if exhausted before or during that drain, stop it without another read and send
the complete shutdown request once. Do not extend/reset the read budget, retry
writes, or read an acknowledgement at the limit. After full shutdown writes,
set `cleanup_error` to `Micro read budget exhausted; cleanup drain incomplete;
shutdown written without acknowledgement` and leave `led_clear_acknowledged=false`.
Preserve an existing primary acquisition error; if cleanup was the first
failure, it becomes the primary error too. A short/failed shutdown write stops
immediately with its actual write error instead. All non-cleanup drains retain
the full fixed-interval contract or fail before their RPC write.

Clear acknowledgement proves an RPC result, not witnessed darkness. Clear failure sets `cleanup_error`, leaves acknowledgement false, and converts prior success to failure. If there was already a primary failure, retain it as `error` while also preserving cleanup error.

Pre-open/descriptor/option failures may retain their current phase rather than `failed` because runner finalization was not reached. Health-check success retains `health_check`.

### 9.6 Supervision and offline replay

Launch same executable's internal diagnostic worker through shared bounded-stdio supervision. Requested timeout:

- Health check: `3s`.
- Normal: `capture_ms +20000ms`.
- Keymap: another `5000ms`.

Reject deserialized output asserting ready/qualified, inconsistent read-only accounting, or more than `128` captures/inputs. Shared supervisor internals are a separate contract.

#### Explicit CLI integration and current platform limits

`micro-probe` accepts only `--capture-ms` followed by unsigned ASCII decimal
`0..30000` (default `2000`) and the valueless `--query`, `--read-keymap`,
`--test-pattern`, and `--health-check` flags. Dependency/conflict rules above
apply regardless of option order. Unknown, duplicate, missing or malformed
options fail before worker launch; there is no port/path/command selector.

Live access uses the shared private `__probe-worker` route with a framed,
complete options object and kind `micro_hid_probe`. Its coordinator uses a
Micro-only static allowlist; the hidden worker's compiled diagnostic registry
also contains health, CT and AKP03 kinds. The callback validates all options,
checks detached, one-process kill-on-close Windows Job containment or the macOS
ARM64 process-group worker capability (platform appendix §8.7), and only
then constructs a native backend. Framing rejects duplicate JSON keys; typed
outcomes/progress reject unknown or missing fields. Results are the complete
outcome schema above, with failed reports on stderr and nonzero CLI status.
Worker failures use `status: worker_error`; no partial success JSON is printed.
Windows supervision can additionally wait up to five seconds for exit
and one second for I/O joining after failure; macOS group cleanup has a two-second
budget per attempt (Drop may retry once) and closes parent pipes synchronously.
Unresolved cleanup fences reuse.
The result policy runs **after** all native sequencing and attempted LED
cleanup. It measures compact serialized bytes without building unbounded output.
Payloads must fit 65280 bytes, reserving 256 bytes within the unchanged 65536-byte
frame for the envelope and maximal worker identity. If the original outcome
cannot fit, set `probe_succeeded=false`, append an explicit response-limit error,
and populate `response_limit`; bounded native primary/cleanup errors, counts and
LED attempt/acknowledgment flags remain intact.

Rebuilding reserves another 1024 bytes for omission metadata. Preserve whole
status then whole keymap values when each fits; otherwise export null and list
`device_status`/`keymap` in `omitted_fields`. Then retain complete observed-input
and raw-capture prefixes in that order, recording exact omitted row counts.
Never clip JSON values, messages or hex strings; a non-fitting input terminates
the retained prefix. Dropped captures also set `capture_truncated=true`.
This is a failed diagnostic export, not successful full observation.

If scalar text or serialization prevents even that report from fitting, a
compact typed failure retains numeric progress and write/cleanup flags, records
omitted descriptive fields (`firmware`, `candidate`, `phase`, `error`,
`cleanup_error` as applicable), and uses phase `response_limit`. Primary errors
over 4096 bytes are explicitly omitted; normal bounded primary errors are
preserved with the limit error appended. No new I/O or cleanup is attempted by
result budgeting. The shared frame encoder also returns a correlated compact
`response_encoding_failed` outcome if serialization/frame encoding fails;
the coordinator reports `frame_limit` rather than unexplained EOF.

`--health-check` bypasses backend construction and all device I/O in the child.
On platforms without the Windows/macOS ARM64 supervisor it returns only the pure health
report in-process; this is not a live-I/O fallback. Native probing requires
`micro-hid`. Windows reads optional exact BusTypeGuid metadata through a bounded
native parent walk across logical HID bus nodes, stopping at missing/unknown
transport and classifying only the documented USB bus GUID. Incomplete scans
or unresolved matching candidates fail closed, not false-unique.
Native interface numbers and
original report descriptors remain unavailable, so live probes fail closed
before transport open/reports. The report-descriptor IOCTL is HID minidriver
traffic, not a supported user-mode collection request; no parsed-data or path
substitute is accepted. All Windows HID access requires a private scoped
contained-child capability and post-open identity/bus revalidation; see
platform appendix §2.5. macOS ARM64 now uses the same-executable bounded process-group worker;
all IOKit access and writes require its private scoped capability and a
same-executable parent. No ordinary in-process native access or live fallback
exists. Exact native property/descriptor evidence remains required and unverified
on hardware. A child-side 50 ms parent-lifetime watchdog can terminate a blocked
main thread on parent death; it is not kernel-enforced parent-death or
escape containment and cannot overcome process suspension/OS-driver stalls. Killing
the child cannot prove LED clearing. No hardware, Windows runtime containment,
or physical input/output acceptance is claimed.

Offline replay requires `1..128` ASCII strings, each `128` hex characters encoding exactly `64` bytes; upper/lowercase hex accepted, whitespace/wrong lengths invalid. Feed fresh framing decoder with time always zero. Return:

- `read_only=true`
- `command_ready=false`
- `messages`: completed JSON objects
- `incomplete_bytes`: retained byte count

No device/feature or RPC admission is required. Trailing incomplete bytes are reported, not failed. Age cannot expire because injected time stays zero.

## 10. Firmware capability and local inputs

### 10.1 Capability object

Fields: `path`, `driver_available`, `platform`, `transport`, `supported_version`, `evidence`.

Unavailable:

- `path="unavailable"`, driver false.
- Platform/transport/version null.
- Evidence `no_verified_update_path`.

Micro:

- `path="official_esp32_usb"`: compatibility enum identifier.
- Driver true only with `micro-firmware` on macOS ARM64.
- Platform `macos_aarch64`, transport `usb`, version `0.6.3`.
- Evidence `manual_reference_verified_automated_acceptance_pending`.

This capability is separate from readiness/qualification and does not enable Bluetooth updating.

### 10.2 Explicit update options

Supply local `image`, `esptool`, exact `port`, new `backup_dir`, and `confirm`.

Validate in order:

1. Exact untrimmed case-sensitive confirmation `INSTALL MICRO 0.6.3`.
2. Absolute image/tool/backup paths.
3. UTF-8 port beginning exactly `/dev/cu.usbmodem` followed by at least one ASCII alphanumeric character and nothing else.

No automatic selection, wildcards, suffix punctuation, whitespace, or empty suffix. Validation failures precede device access. Existing-directory rejection happens later, still before qualification/bootloader.

### 10.3 Local artifact input requirements

These are integrity requirements for files already supplied locally, **not download instructions**:

| Input | Exact bytes | SHA-256 |
|---|---:|---|
| Merged firmware `0.6.3` image | 2,214,944 | `1d8bd545b9047182d7112dd86b14f6c636bad3834725ee0a6eca7cacee0ec6a7` |
| Standalone macOS ARM64 tool `5.6.0` | 13,920,608 | `fe300417054386e8c15f916216924accb05d0132411bfce3b223ad621f0de6d9` |

Require regular file/exact size before opening and on opened handle. On macOS open no-follow/nonblocking. Read at most expected+1 bytes, require exact result, compare full lowercase SHA-256. Reject substitutions, wrong versions, truncation, growth, or altered bytes. No PATH lookup or implicit download.

Image extent additionally requires first byte `0xE9`, exact size, and end rounded up to `4096`-byte sector not beyond `0x810000`. This is not a full image/partition parser; the hash provides the stronger pin.

| Flash region | Offset | Size/end |
|---|---|---|
| Whole flash | `0` | `0x1000000` =16,777,216 bytes |
| Partition sector | `0x8000` | `0x1000`; end `0x9000` |
| Settings/NVS/filesystem span | `0x810000` | `0x220000` =2,228,224; end `0xA30000` |
| Merged write | `0` | 2,214,944; sector-rounded end `0x21D000` |

Do not invent internal settings subpartitions. Compare the entire partition sector before writing and entire preserved span afterward.

### 10.4 Offline plan schema

Requires `micro-firmware`; the offline function itself is not restricted to macOS ARM64. Verify both local pins and extent, without device access or update-option validation.

Successful object fields:

- `read_only=true`
- `device_access=false`
- `command_ready=false`
- `firmware_update`: Micro capability object
- `artifact_verified=true`, `tool_verified=true`
- `confirmation="INSTALL MICRO 0.6.3"`
- `release`: independently chosen provenance locator string for reviewed `0.6.3`
- `steps`: seven ordered strings:
  1. Verify local artifacts and qualified USB identity.
  2. Enter bootloader once; bind explicit serial port to same location.
  3. Check ESP32-S3, 16MB, disabled security features.
  4. Read/verify full backup; compare partition sector.
  5. Write only reviewed merged image at zero.
  6. Verify image and unchanged settings.
  7. Watchdog reset; verify same-location USB firmware `0.6.3`.

A provenance locator does not authorize downloading. Without feature return failure stating offline validation needs `micro-firmware`, no device access.

## 11. Firmware transaction and recovery

### 11.1 Result schema/phases

Outcome rejects unknown fields:

- `update_succeeded`: boolean.
- `command_ready`, `hardware_qualified`: always false.
- `report`: UpdateReport.
- `error`: string/null.

UpdateReport rejects unknown fields:

- `phase`: string.
- `failure_phase`: string/null.
- `bootloader_entry_attempted`, `backup_verified`, `flash_write_attempted`, `firmware_verified`, `settings_preserved`: booleans initially false.
- `running_version`: string/null.

Start `validating_options`; then:

`preflight` → `preparing_private_backup` → `tool_version` → `qualifying_usb_target` → `bootloader` → `security` → `flash_size` → `reading_backup` → `persisting_backup` → `verifying_backup` → `writing_firmware` → `verifying_firmware` → `verifying_settings` → `resetting` → `verifying_running_firmware`.

Success sets phase `complete`; returned error copies current phase to `failure_phase`, then phase `failed`. Preserve attempted/verified flags independently.

### 11.2 Preflight and recovery directory

Native worker requires `micro-firmware` on macOS ARM64 and process-group ID equal to PID; direct unsupervised worker invocation is rejected. Unsupported worker calls validate options first, then fail without device access.

Acquire shared lease, verify local files/extent, and set private worker umask `0077`. Create **new** backup directory mode `0700`, never reuse/overwrite; canonicalize it. Create-new, write, and sync private copies:

- `firmware.bin`, mode `0600`.
- `esptool`, mode `0700`.
- `tool-tmp` directory, mode `0700`.

After transaction returns, best-effort remove only copied tool/image and empty tool staging directory. Retain `full-flash.bin`, `SHA256SUMS`, `settings-after.bin` if produced, and logs. Early preparation errors or forced termination may leave staging files. Never remove recovery evidence because of failure.

### 11.3 Qualification and one-shot bootloader

First execute pinned tool's version command. Accept a trimmed output line exactly `5.6.0` or `esptool v5.6.0`.

Query system USB-host-device registry as structured plist, environment cleared, timeout `5s`, each stdout/stderr bounded to `4MiB`, top-level array required. Require exactly one application VID/PID USB device and a nonzero unsigned `locationID`.

Open descriptor-qualified USB vendor HID. Maintenance version query:

- Request ID101 `sys.version`, exact-64-byte writes.
- Four-second read budget, at most `128` nonempty reads, `50ms` slices.
- Ignore report IDs `1..4`; require other reports length `64`.
- Frame-decode; match exact JSON number value `101`, not ordinary float coercion.
- Any matching error key rejects; require string `result.version`.

Accept only exact starting versions `v0.1.50` or `0.6.3`. Reject `v0.6.3`, unprefixed `0.1.50`, earlier/later unreviewed versions. An already-`0.6.3` device may update. Requery registry and require same unique physical location.

Set bootloader attempted, then bypass ordinary allowlist only here to send exactly one report containing:

`{"method":"sys.bootloader","params":null,"id":102}`

Require one frame and one count-64 write. No retry and no RPC acknowledgement wait. Close HID.

Poll every `250ms` for up to `30s` for the supplied serial port beneath a USB registry device with VID `0x303A`, bootloader PID `0x1001`, exact saved location, and descendant `IOCalloutDevice` equal to the explicit path. Recurse child dictionaries no deeper than depth `32`. Wrong location/device/port fails binding; no automatic alternative.

### 11.4 Exact tool invocation

Reverify copied tool length/hash before **every** invocation. Except Version, requery registry and require the bound bootloader port still belongs to the qualified USB location.

Execute workspace-local tool, inherited environment cleared:

| Variable | Value |
|---|---|
| `PATH` | `/usr/bin:/bin:/usr/sbin:/sbin` |
| `TMPDIR` | private workspace's `tool-tmp` |
| `LC_ALL`, `LANG` | `C` |
| `TERM` | `dumb` |
| `NO_COLOR` | `1` |

Except Version, prefix arguments:

`--chip esp32s3 --port <port> --before no-reset --after <action> --connect-attempts 1`

After action is `no-reset`, except Reset uses `watchdog-reset`.

| Operation | Remaining arguments | Timeout |
|---|---|---:|
| Version | `version` | 10s |
| Security | `get-security-info` | 60s |
| FlashId | `flash-id` | 60s |
| ReadBackup | `read-flash 0 0x1000000 <workspace>/full-flash.bin` | 240s |
| VerifyBackup | `verify-flash 0 <workspace>/full-flash.bin` | 240s |
| WriteImage | `write-flash --flash-mode keep --flash-freq keep --flash-size keep 0 <workspace>/firmware.bin` | 180s |
| VerifyImage | `verify-flash 0 <workspace>/firmware.bin` | 120s |
| ReadSettings | `read-flash 0x810000 0x220000 <workspace>/settings-after.bin` | 120s |
| Reset | `read-mac`, with watchdog-reset after-action | 60s |

Null stdin; bound stdout/stderr independently to `4,194,304` bytes. Concatenate stdout followed by stderr into named logs `Version.log`, `Security.log`, `FlashId.log`, `ReadBackup.log`, `VerifyBackup.log`, `WriteImage.log`, `VerifyImage.log`, `ReadSettings.log`, `Reset.log`. Nonzero exit fails with private-log/no-retry diagnostic; successful output must be UTF-8. Timeout/output-collection failure may precede log creation.

The explicit chip option and tool success enforce the chip contract; there is no separate chip-description text parser.

### 11.5 Ordered flash gates

1. **Security:** Require trimmed lines exactly `Secure Boot: Disabled`, `Flash Encryption: Disabled`, `Flags: 0x00000000 (0b0)`. Reject if output anywhere also contains `Secure Boot: Enabled` or `Flash Encryption: Enabled`.
2. **Size:** All trimmed lines beginning `Detected flash size:` must be exactly the singleton `Detected flash size: 16MB`; duplicates/missing/other sizes fail.
3. **Backup:** Read full `16MiB`, require exact regular-file size, and compare `[0x8000,0x9000)` against reviewed image's partition sector. Difference stops before writing.
4. **Persist:** Compute lowercase backup SHA-256; write `SHA256SUMS` as digest, two spaces, `full-flash.bin`, LF. Sync backup/checksum and workspace directory. This alone does not set backup verified.
5. **Verify backup:** Run full verify; reread backup and staged image and compare bytes against in-memory originals. Only then set `backup_verified=true`.
6. **Write:** Set `flash_write_attempted=true` immediately before one write at offset zero. Preserve flash mode/frequency/size. No erase-all, settings write, or retry.
7. **Verify image:** Tool verify success sets `firmware_verified=true`.
8. **Verify settings:** Read exact preserved span to `settings-after.bin`; require byte equality with backup `[0x810000,0xA30000)`. Only then set `settings_preserved=true`. Mismatch fails without resetting.
9. **Reset:** Invoke read-mac with watchdog-reset after-action. Failure remains failure.
10. **Running version:** Poll up to `30s` at `250ms` for unique application device at same location; reopen qualified USB and query version. Store string, require exactly `0.6.3`. A present same-location device whose opening/query fails returns immediately; no repeated query retries.

Only all ten gates yield success. `v0.6.3` after update fails exact final comparison.

### 11.6 Failures, cancellation, supervision

Stop at first error. No automatic flash retry, full-flash restore, settings restore, rollback, forced reset, fallback bootloader mechanism, or recovery command.

Before WriteImage, failure means no flash write attempt. Failed write, image verification, or preservation after writing must not proceed to reset; leave bootloader state for deliberate recovery. Retain durable backup/checksum/logs. An independently authorized operator procedure must decide recovery.

Supervisor:

- Install SIGINT/SIGTERM cancellation handlers and restore them afterward.
- Start same executable's maintenance worker as new process-group leader.
- Limit total to `1800s`; stdout/stderr independently to `65536` bytes.
- Poll every `20ms`.
- Keep group leader unreaped long enough to terminate owned group before PID reuse; clean descendants even after successful leader exit.
- On timeout/cancel kill owned group and wait; retain recovery directory.
- After exit allow `1s` for each output pipe to close.
- Nested tool/registry commands inherit worker group; worker cancellation kills them together. Individual operation timeouts kill their immediate child.
- Unsuccessful/interrupted worker is failure, not partial-JSON success.

Strictly deserialize result and reject ready/qualified true. Claimed success additionally requires backup verified, flash write attempted, firmware verified, settings preserved, and final version exactly `0.6.3`. These are explicit supervisor invariants, not independent reexecution of the transaction.

## 12. Deterministic acceptance scenarios

### Protocol and enumeration

1. ID101 version request: header `06 02 2f`, 47-byte JSON, 14 zeros; exact roundtrip.
2. Fragment object containing a result string of 59 ASCII `x` plus 100 snowmen; split UTF-8 across chunks and recover exactly.
3. Empty-buffer CR/LF chunk does not start age; valid message ten seconds later works.
4. Partial `{"id"` at t10; `:` at t15 remains valid incomplete; `1}` at t15.001 fails with buffer length 8 retained.
5. `[6,3,0]` unknown channel; `[5,2,0]`, count62, and truncated payload invalid frame.
6. Channel1 interleaving leaves partial JSON intact. `[1]` fails nonobject after clear; next object succeeds. `{"a":NaN}` remains buffered.
7. Rejected `fs.write`/`fs.read(null)` do not consume ID101. Interleaved AG notification survives reply. Numeric `101.0` matches ordinary RPC; string `"101"` does not. `error:null` rejects.
8. Sent t10 deadline t14; check13.999 passes, check14 fails. 128 queued inputs pass; 129th fails; unrelated notification types consume no capacity.
9. Verify all admitted/rejected firmware spellings above, distinguishing unsupported from malformed-result stop disposition.
10. MI_02 one path/mixed case pass; MI_01, empty suffix, duplicate paths fail; invalid UTF-8 suffix retains replacement character.
11. Caps success tuple passes; any single mismatch fails. Write12 fails short-write.
12. Conflicting path case variants deduplicate to `["chatgpt.exe","input.exe"]`; `.old` does not match.
13. Descriptor fingerprints pass only with required identity/bus/interface; changing report count63 to62, last byte, PID, usage, or transport fails before output.

### Inputs and calibration

1. AG02 boolean press t1 selects3; release1.1; integral-float press1.2 debounces and holds; release1.25; press1.3 selects3.
2. Active `[2,4]`, no/unlisted selection: CW2, CC4; from4 CW2; empty list none. CW1.29 after accepted1.0 rejected, CW1.3 accepted; opposite direction independently eligible.
3. ACT07 press1.0, release1.2, press1.4 rejected but held; press1.6 still rejected; release1.7/press1.7 accepted.
4. ACT10 press1.0 emits Mic; ACT11 press1.02 does not; release both then ACT11 at4 emits. Early new physical gesture within400ms is suppressed and held.
5. Angles `{up:.75,right:0,down:.25,left:.5}`: `.74/1` up; held `.75/1` none; center then `.99/1` right.
6. Distance `.64` no emit, `.65` emit/disarm, `.25` not rearm, `.249` rearm. Angle `.875` ties up/right and picks up. Angle1 normalizes0/right.
7. Empty calibration updates telemetry/no key and remains armed. Boolean telemetry, distance1.5, missing angle, or string fails reconnect.
8. Partial duplicate angles valid; complete duplicates invalid. Wrapped `.94/.09/.5/.75` rejects floating overlap.
9. Capture age2/distance.65 passes; age2.001 or distance.64 fails. Future timestamp and distance true passes; angle1 fails.
10. Only exact `CALIBRATE` authorizes. `" Q "` quits; `"quit"` does not.
11. Radial with `k=AG01,act=1` may also select2 in legacy decoding; strict adapter rejects it.
12. Strict adapter ignores bool/float acts, ACT10, DPAD_UP, ENC_CLK; preserves generation/time for accepted AG and converts integer ENC_CC2 to turn-1.

### LEDs and health

1. Legacy six-color frame gives packed `[0,16777215,2126079,53344,16755200,16724016]`, IDs0..5, effect1/speed.2.
2. Selected1/brightness.4 gives `[.4,.2,.2,.2,.2,.2]`. Shutdown black retains brightness.25/effect1/speed.2.
3. `[off,idle,off,error,off,waiting]` yields active `[2,4,6]`; Unknown is active/distinct.
4. Semantic encoder refuses wrong scope, ID6, bad brightness, absent qualification, zero-speed pulse, 999ms smooth period, token/ID/speed mismatch.
5. now100/heartbeat99/connectedtrue/config loaded is ready even with D-pad warning or last-error warning.
6. Heartbeat90/101/stoppedtrue stale, USBnull, not ready. Heartbeat100 or90.01 accepted. Fresh missing connected gives false.
7. Missing file → missing-heartbeat diagnostic; BOM valid; connectednull/live1 invalid.
8. Age.25 formats.2; future age-.04 formats-0.0. Missing stop reason differs from explicit null.
9. ESC/newline sanitized to spaces, ANSI bracket text retained. Calibration predicate accepts stopped `[false]`, rejects `"false"`; normal validation rejects both.

### Diagnostics

1. Passive short keyboard report plus AG02/ACT08/radial yields zero writes, ignored keyboard count1, slot3, command name, telemetry/no D-pad action.
2. Query+keymap+capture0 issues version/status/fs.read only. Missing query fails before output.
3. Query+pattern+capture0 issues version/status/pattern/shutdown, drains >=50ms between calls, preserves input, acknowledges clear, remains not ready/unqualified.
4. Old firmware query records version then stops after one request.
5. Short third write during LED transaction stops immediately without retry or cleanup append.
6. Silent version times out after one request. Clear rejection turns otherwise successful probe into failure.
7. 129 vendor reports during long passive capture store128, truncate, stop successfully, zero output.
8. Replay full fragmented object gives zero incomplete bytes; first fragment gives positive incomplete bytes without failure; `"06"` or129 strings rejected.

### Firmware

1. Wrong confirmation/relative input/bad port fails with no device access. Artifact alteration/length mismatch prevents bootloader entry.
2. Synthetic approved-header image with partition sector filled `A5`, backup filled `5A` except identical partition, exercises Version→Security→FlashId→ReadBackup→VerifyBackup→WriteImage→VerifyImage→ReadSettings→Reset, with separate qualify/bootloader/reopen phases.
3. Protected security, partition mismatch, or failed backup verify never write.
4. Failed write/image verify/changed settings byte never reset or roll back; one write attempt maximum, backup retained.
5. Wrong/duplicate size line, unreviewed starting version, changed location, wrong bootloader PID/port, or altered tool fails its gate.
6. Final `v0.6.3` fails with recorded string; exact `0.6.3` completes with all verification flags.
7. Timeout/output overflow/cancellation fails boundedly and retains backups. Supervisor rejects forged readiness/qualification or incomplete success flags.

## 13. Integration dependencies and physical acceptance

Connect this device behavior to shared six-slot/status/action/settings contracts; canonical semantic RGB tokens; presentation clocks and connection generations; scoped capability/effect evidence; session cursor/selection; structural authorization/pending-action checks; configuration loading; health snapshots; and bounded diagnostic supervision.

Live foreground admission, voice operation, continuously published controller
status, automatic session discovery, and session-command delivery are not
implemented by these helpers. Preserve their unavailable boundaries rather
than treating them as extra reconstruction requirements.

Do not equate the legacy and strict input adapters. Do not derive Micro LED Off from another device's screen palette. The exact Unknown RGB must come from the shared semantic contract. Feature packaging must provide required native dependencies; do not infer feature dependency relationships from platform guards alone.

Not implemented/qualified by this contract:

- A production continuously connected driver dispatching session commands.
- Generic Linux HID support.
- Windows production owner-lock integration inside native diagnostics.
- Bluetooth updating.
- Alternate chip/flash sizes, firmware images, starting versions, tool versions.
- Automated restoration or arbitrary filesystem/layer edits.
- Encoder-press, Mic, or calibrated-stick semantic normalization beyond explicitly listed behavior.
- Physically proven native steady/pulse output without external qualifications.

Physical acceptance must separately verify intended application layer, ordinary firmware 0.6+ behavior, report/channel IDs, response echoes, key/action names, radial range/orientation, Windows caps, wide-key switch coalescing, actual LED color/steady/smooth-cycle quality, disconnect/reconnect behavior, safe shutdown, maintenance preservation, and session-command end-to-end operation. Measured descriptors, manual transaction evidence, or synthetic tests alone must not change ready/qualified claims.
