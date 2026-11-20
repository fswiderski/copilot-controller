# MX Creative Console evidence block

Status: clean-room, read-only macOS ARM64 evidence collection plus a
semantically read-only HID++ display-information query. This does not qualify
the hardware, provide actions, or establish controller bindings.

## 1. Scope and non-goals

`mx-creative-evidence` is an explicitly invoked, finite diagnostic. It uses the
existing same-executable supervised HID worker and the `mx-creative-hid` Cargo
feature. The worker may enumerate two exact collections, open them with IOKit
option `0`, and read input only. The public worker route is dedicated to this
single callback.

The command does not:

- write HID reports, seize devices, or request access;
- use `IOHIDRequestAccess`, global keyboard observation, event injection, or
  global key state;
- expose button names, terminal text, keyboard-state capture, serial numbers, or
  lighting/display evidence; exact device-scoped mouse/system/consumer reports
  listed below are byte-level diagnostic evidence only;
- connect captures to controller actions.

Permission denial, absent evidence, unknown transport, non-unique enumeration,
open failure, changed identity, and descriptor mismatch are failures. No
successful empty inventory is synthesized.

## 2. Exact evidence

The two expected services are:

| Role | VID/PID | Transport | Primary usage | Input / output | Extra identity |
|---|---:|---|---|---:|---|
| Keypad | `0x046d/0xc354` | USB | `0xff43/0x1a02` | 32 / 4095 | product `MX Creative Keypad`, LocationID `0x02110000` |
| Dialpad | `0x046d/0xbc00` | Bluetooth | `0x0001/0x0002` | 20 / 20 | product `MX Dialpad` |

The macOS IOKit transport-name normalization maps only exact case-insensitive
`USB`, `Bluetooth`, and `Bluetooth Low Energy` values. Prefixes and other
unknown strings remain `Unknown` and fail closed.

The complete original descriptors are required byte-for-byte:

| Role | Bytes | SHA-256 |
|---|---:|---|
| Keypad | 140 | `d6faec8e21db00518c67b4446875b6bdb9d85988d05d888641da069fb302bc54` |
| Dialpad | 97 | `49ea87e24b5049ea9e7e01dd750754aad63f37f51ce8191fee1c1ff7ae5d2cac` |

The descriptor hex is pinned in
`src/devices/mx_creative/probe.rs`; reconstructed or parsed-equivalent
descriptors are not accepted. A dedicated native callback filter retains only
these normalized input frames:

| Role | Report ID | Total bytes | Descriptor field |
|---|---:|---:|---|
| Keypad | 4 | 2 | generic desktop page `0x0001`, system-control usage `0x0080` |
| Keypad | 5 | 2 | consumer page `0x000c`, consumer-control usage `0x0001` |
| Keypad | 17 | 20 | vendor page `0xff43`, usage `2`, 19 byte elements |
| Keypad | 19 | 32 | vendor page `0xff43`, usage `8`, 31 byte elements |
| Dialpad | 2 | 8 | generic desktop page `0x0001`, mouse usage `0x0002` |
| Dialpad | 17 | 20 | vendor page `0xff43`, usage `2`, 19 byte elements |

On descriptor mismatch the failure includes only the selected collection's
numeric usage/report-cap identity, sanitized `iokit:<registry-id>` paths, whether
the selected and source service IDs match, the CoreFoundation property class,
and actual/expected lengths and SHA-256 values. It never includes descriptor
bytes. The `CFData` source and same-service check distinguish a real bytes
variance from selecting a different collection or receiving another property
type. Any nonmatching evidence remains rejected pending independent review.

The dialpad descriptor does not define a vendor input report ID 20. Report 20
and all other report IDs or lengths are not retained as captures. Output
contains only selected report bytes, report ID, exact field page/usage, and
numeric element values. Standard mouse/system/consumer bytes are evidence only;
they are not interpreted as global input or controller actions, and field
meaning is not yet inferred.

The callback and JSON response are bounded: 64 captures, 32 bytes per retained
report, and a maximum 30-second capture plus five-second setup budget. A
shorter capture may be selected. Identity checks repeat after open.

## 3. Build and invocation

On macOS ARM64:

```sh
cargo build --release --features mx-creative-hid
target/release/copilot-controller mx-creative-evidence --capture-ms 30000
```

Do not run the hidden worker argument directly. Do not start the command until
all captures below can be completed in one 30-second interval. The command does
not request or change Input Monitoring permissions.

## 4. Physical evidence sequence

Use the physical labels/order rather than interpreting JSON values:

1. Start the command and wait until the worker is open (before touching it).
2. On the keypad, press and release button 1, then wait 500 ms.
3. Repeat in top-to-bottom, left-to-right order for buttons 2 through 9.
4. Press and release the left page button, wait 500 ms.
5. Press and release the right page button, wait 500 ms.
6. On the dialpad, rotate one detent clockwise and release, wait 500 ms.
7. Rotate one detent counter-clockwise and release, wait 500 ms.
8. Leave both devices untouched until the command exits.

For an initial mapping run, one 30-second sample is sufficient. Keypad controls
1–9 are observed on vendor report 19 at values index 5: a press produced the
individual control value and release produced 0. The two page controls are
evaluated from system/consumer reports 4/5, but no page-control evidence has
yet been observed, so their mapping remains unqualified. Dial movement is
observed on the exact mouse report 2: interpreting the final report byte as
signed `i8`, clockwise movement produced positive values 1–13 and
counter-clockwise movement produced negative values encoded as 255–245. No
report is captured as global keyboard/mouse input. Save the JSON
locally only if its contents are needed for review; it intentionally contains
raw proprietary report bytes.

## 5. Physical acceptance gate

Passing this diagnostic proves only that the exact two native services were
found, descriptor bytes matched, opens used option `0`, the worker remained
bounded, and selected device-scoped evidence frames were observed. It does **not**
prove:

- page-button bytes or semantics;
- dial direction, detent accumulation, rollover, repeat, or release semantics;
- button debounce, simultaneous presses, or page-button state;
- permission behavior across reconnect/logout/boot;
- display/lighting behavior;
- action readiness, controller qualification, or safe user action wiring.

### Observed physical evidence

The completed exact-device capture reached the 64-frame capture bound. Recorded
facts are limited to:

- keypad buttons 1–9: press transitions on report ID 19, distinguished at
  `values[5]`; release transition is `0`;
- dialpad clockwise: report ID 2 final Consumer AC Pan byte at report index 7,
  interpreted as signed `i8` (the nominal wheel byte at index 6 remains zero)
  yielded positive movement 1–13;
- dialpad counter-clockwise: the same byte yielded negative movement encoded
  as unsigned bytes 255–245;
- no page-button report was observed, so no system/consumer page-button claim
  is made;
- capture truncation at 64 frames reflects the bounded evidence policy, not
  proof that later reports are absent.

All nine keypad controls are mapped only as raw evidence. Page controls and
other standard-report semantics remain pending separate physical review. Any
ambiguous, extra, or non-deterministic report blocks qualification. No
controller action mapping is authorized by this block.

## 6. Semantic display-information discovery

`mx-creative-display-info` is a separate, finite worker route. It is available
only on macOS ARM64 with `mx-creative-hid`, and it selects exactly the USB
`046d:c354` `MX Creative Keypad` vendor collection at
`0xff43/0x1a02`, LocationID `0x02110000`. The complete original report
descriptor must match the pinned 140-byte SHA-256 value in section 2 before
open and after open.

The child authority is `MxCreativeDisplayInfo`, and its route has no action or
streaming callbacks. It opens IOKit with nonexclusive option `0`, filters input
to report ID `0x11` at exactly 20 normalized bytes, and exposes no arbitrary HID
write method. Internally it can submit only the following typed GET queries,
each on one 20-byte report `11 ff feature function/swid ...`:

| Query | Feature/function | Purpose |
|---|---|---|
| `IRoot GetFeature` | index `0x00`, function `0` | Discover exact IDs `0x0008`, `0x8040`, `0x19a1`, and `0x1b04` |
| KeepAlive range | discovered `0x0008` index, function `0` | Read the established minimum/maximum timeout range |

Every request has a session-unique software ID. Successful replies must
correlate on device index, report ID, exact feature index, function, and
software ID; HID++ error replies use their correlated software ID and the
`0xff` error feature index. Unrelated software IDs are ignored only until the
per-request two-second deadline. There are no retries. Four discovery requests
are sent; the fifth KeepAlive range request is sent only when discovery
returned a nonzero index. The whole worker budget is below 30 seconds.

Despite the read-only semantics, HID++ requests physically send output reports.
This command therefore must not be described as passive capture. It never
sends a SET, function 1 to KeepAlive, key diversion, keepalive activation,
brightness change, LCD output, firmware request, reset, or arbitrary frame.
In particular, brightness, display-state/geometry, and controls metadata
functions are not queried: their feature-table presence is reported while
their `metadata_status` remains explicitly `*_not_authorized`. These GET
contracts require a separately corroborated protocol and authorization review.

JSON output includes only device numeric identity and descriptor digest,
feature IDs/indices/status, HID++ error codes, the KeepAlive millisecond range,
bounded request count, and elapsed time. It does not include serials, registry
paths, product strings, raw requests, raw replies, or other report payloads.
Feature index zero means absent; bounded HID++ unsupported/unknown errors are
also represented as per-feature absence rather than a failure of the entire
discovery. Transport, correlation, framing, timeout, or descriptor failures
remain command failures.

Build and invoke after review, without touching display pixels:

```sh
cargo build --release --features mx-creative-hid
target/release/copilot-controller mx-creative-display-info
```

This phase does not qualify the display path and provides no direct-write icon
authority. It supplies a fail-safe feature map and KeepAlive range only.
