# Platform and diagnostic services

This appendix specifies platform observation, clocks, lifecycle invalidation, cooperative runtime ownership, disposable diagnostic workers, and target/run correlation for `copilot-controller`.

It is a behavioral contract, not an implementation layout. It distinguishes implemented diagnostics from future authority and physical qualification. Acceptance scenarios describe expected behavior; they do not assert that physical-device or native-desktop qualification has occurred.

## 1. Authority boundary

**Observation is never permission.** None of the following establishes terminal admission or authority to act:

- A compiled capability.
- A process basename, PID, window, foreground sample, desktop sample, or console membership.
- Same-user/logon token comparison or file-identity comparison.
- UI structure, keyboard-focus metadata, or a text-control count.
- A discovered serial interface.
- A runtime lock, a fresh counter sample, an opaque target ID, or a correlated worker response.

This subsystem does not enter terminal text, inject keys, activate windows, change focus, retrieve terminal text, or execute commands in a selected terminal. It does not prove exact pane/tab ownership or split-pane exclusion. There is no key-injection fallback for semantic same-run Stop.

Ordinary diagnostics never use UI Automation or attach to a console. Explicit helper-only diagnostics may inspect UI metadata or attach their own fresh helper to a console, but never read UI text, titles, console screen buffers, command lines, or terminal input.

Serial discovery queries device metadata and registry values without opening a serial port.

All ordinary reports retain `read_only = true`, `command_ready = false`, and, wherever present, `w2_qualified = false`. Every device inventory entry retains `runtime_qualified = false`. Positive diagnostic evidence must not change these flags.

`w2_qualified` is the diagnostic schema's native-qualification marker. It remains false throughout this foundation; it is not another permission level.

## 2. Capability inventory: compiled versus qualified

### 2.1 Envelope

The offline inventory has:

| Field | Meaning |
|---|---|
| `schema` | `1` |
| `evidence` | `compile_time_only_not_native_or_physical_acceptance` |
| `os`, `architecture` | Platform identifiers |
| `read_only` | Always `true` |
| `command_ready` | Always `false` |
| `platform` | Platform-service support |
| `hosts` | Integrated-host and default native-host support |
| `devices` | Compiled device-discovery/diagnostic transports |
| `maintenance` | Imported maintenance-capability information |
| `actions` | Unavailable until independent authority gates pass |

Each support entry is one of:

```json
{"status":"compiled","prerequisite":"static prerequisite identifier"}
{"status":"unavailable","reason":"static reason identifier"}
```

`compiled` means backend code is included. Inventory performs no native probe, permission check, enrollment, device opening, qualification, or grant.

### 2.2 Platform matrix

| Service | Windows | Linux | macOS | Other OS |
|---|---|---|---|---|
| Shared native clock | Compiled | Compiled | Compiled | Unavailable |
| Native lock/sleep/logout subscriptions | Unavailable | Unavailable | Unavailable | Unavailable |
| Safe private storage | Compiled prerequisite | Compiled prerequisite | Compiled prerequisite | Unavailable |
| Cooperative runtime ownership | Compiled | Compiled | Compiled | Unavailable |
| Bounded diagnostic children | Compiled | Compiled | Compiled | Unavailable |
| Durable action-writer quiescence | Unavailable | Unavailable | Unavailable | Unavailable |
| Process/window/desktop/serial observations | Implemented | Unsupported | Unsupported | Unsupported |
| Explicit process/UI/console qualification probes | Implemented | Unsupported | Unsupported | Unsupported |
| Framed, Job-contained diagnostic workers | Implemented; private live-HID scope | Unsupported | Unsupported | Unsupported |
| Framed HID/health workers | Job-contained; optional HID metadata, live probes fail closed | Unsupported | Process-group, compiled ARM64 | Unsupported |

Clock prerequisites: nonnil runtime epoch, same native source, same boot.

Storage prerequisites are advertised as:

- Windows: explicitly selected local fixed non-reparse root.
- Linux: explicitly selected private local ext4/XFS/Btrfs root.
- macOS: explicitly selected private APFS root without extended ACL.

The shared safe-root primitive must enforce actual filesystem policy. This appendix specifies its use, not its internals.

Ownership requires one approved per-user authority anchor independent of the configuration root. Free advisory locking does not prove writer death or safe takeover.

Diagnostic-child prerequisites differ:

- Windows stdio capture supervises a direct child; the stronger suspended-Job worker is separate.
- Linux/macOS capture uses process groups without a parent-death or escape guarantee.

Native lifecycle acceptance and confirmed death of all action writers remain separate, unimplemented gates.

### 2.3 Host capability semantics

The **integrated-host** entry currently advertises all of the following as unavailable:

- Discovery: versioned host-contract interoperability is required.
- Authentication: an owned relay and native secret store are required; no plaintext fallback.
- Association: host-supplied recipient metadata is required.
- Confirmed presentation: host confirmation is required.
- Same-run Stop: exact same-run recipient, Stop-only grant, and pre-gesture expiry are required.

The **default native-host** entry:

- Advertises Windows read-only discovery only.
- Does not authenticate a cooperating host.
- Does not prove the exact interactive recipient.
- Does not equate foreground/focus with confirmed presentation or admission.
- Has no semantic same-run Stop contract or key-injection fallback.

On Linux/macOS, reviewed host-specific native discovery and permissions remain required.

### 2.4 Device inventory semantics

Device labels are neutral capability classes, not provenance assertions:

This describes the emitted `portable_inventory()` fields, not a broader design
matrix or native scan. Standalone discovery is always unavailable. On macOS
ARM64, the reason `metadata_only_inside_explicit_hid_probe` is emitted only for
the matching enabled family feature; it means metadata can be obtained inside
an explicitly opted-in supervised HID probe, not that discovery ran.

| macOS ARM64 build | Primary USB / related Bluetooth (`micro-hid`) | Alternate second revision (`akp03-hid`) |
|---|---|---|
| Default (neither HID feature) | Discovery unavailable: `feature_disabled`; diagnostic unavailable | Discovery unavailable: `feature_disabled`; diagnostic unavailable |
| Micro only | Metadata-only discovery reason; compiled diagnostic prerequisite | Discovery unavailable: `feature_disabled`; diagnostic unavailable |
| AKP03 only | Discovery unavailable: `feature_disabled`; diagnostic unavailable | Metadata-only discovery reason; compiled diagnostic prerequisite |
| Both HID features | Metadata-only discovery reason; compiled diagnostic prerequisite | Metadata-only discovery reason; compiled diagnostic prerequisite |

Disabled-family diagnostic transport retains reason `device_backend_not_integrated`.
The original alternate profile stays discovery-unavailable with that reason and
diagnostic-unavailable with `decode_only_original_profile` in every build.
The serial row stays unavailable with `device_backend_not_integrated`. Windows
and Linux neutral device-inventory rows are unchanged; Windows CT CLI availability
is reported separately as `ct_serial_probe_available`, not inferred from these
rows. `micro-firmware` does not enable HID metadata or probing.

Compiled transport still requires the exact profile/native evidence, permissions,
private worker capability and human opt-in. Every row is runtime-unqualified.

Maintenance for the primary USB controller imports the maintenance subsystem's capability. Other controller families advertise unavailable maintenance. This inventory does not perform maintenance.

Aggregate action support remains unavailable until provider, host, durability, lifecycle, device timing, and writer-quiescence gates pass.

### 2.5 Windows HID evidence and residual scope

With `micro-hid` or `akp03-hid`, Windows x64/ARM64 can collect optional
metadata only inside a validated, explicitly opted-in HID worker callback.
A private thread-bound, per-request capability checks the detached,
one-process, no-breakaway, kill-on-close Job before backend construction and
native operations. Health and ordinary in-process callers receive no capability;
expiry or failed containment revalidation denies further access. OS/driver
stalls are bounded only by the existing disposable worker supervision, not
by an in-process fallback or a guaranteed per-call timeout.

| Artifact | Native source and exact acceptance | Remaining limitation |
|---|---|---|
| Bus | `SetupDiGetDeviceInterfaceDetailW` supplies the collection's `SP_DEVINFO_DATA.DevInst`; `CM_Get_DevNode_PropertyW(DEVPKEY_Device_BusTypeGuid)` must succeed with exactly `DEVPROP_TYPE_GUID` and 16 bytes. An exact `GUID_BUS_TYPE_HID` permits `CM_Get_Parent` traversal, bounded to 16 nodes. At the first non-HID bus, only `GUID_BUS_TYPE_USB` (`9d7debbc-c85d-11d1-9eb4-006008c3a19a`, Wdmguid.h) maps to USB. Other valid GUIDs map to `Unknown`; an explicitly missing property remains unavailable and stops traversal. | The collection may report only a logical HID bus. Never search past an unknown/missing transport for USB ancestry (for example a Bluetooth adapter); no string/path/hardware-ID/usage inference. Wrong type/size, transient property/parent failures, cycles and walk-cap exhaustion abort enumeration/open. Bluetooth classification is not implemented without a documented bus-GUID contract. |
| Interface number | Unavailable (`None`). | No documented exact `bInterfaceNumber` source was confirmed for this user-mode collection path. Do not parse `mi_` or inspect `HIDP_CAPS` reserved fields. |
| Original report descriptor | `EvidenceUnavailable(RawReportDescriptor)`, without issuing a descriptor IOCTL. | Microsoft lists `IOCTL_HID_GET_REPORT_DESCRIPTOR` under **HID Minidriver IOCTLs**, sent only by HIDClass to minidrivers, not by user-mode applications to collection handles. `IRP_MJ_DEVICE_CONTROL` on the individual IOCTL page does not change this contract. Collection-descriptor/preparsed-data APIs return parsed data, not the original bytes; reconstruction is prohibited. |

Bus reads use a fixed 16-byte buffer, never an allocation or retry based on an
untrusted required size. The entire observed devnode/GUID ancestry prefix is
retained. `CR_NO_SUCH_VALUE` is explicit missing-property evidence, not a
transient failure and not permission to walk farther. Other CM failures,
including a disappeared devnode/parent, retain their raw `CONFIGRET` code in
`HidError::Os` with the enumeration/open operation. SetupAPI/file failures retain
Win32 codes; failed `HidP_GetCaps` retains its signed native status.
SetupAPI scans at most 4096 interfaces, caps each
detail buffer at 4096 bytes and validates a terminated UTF-16 path within the
returned extent and the 512-byte metadata limit. Only `ERROR_NO_MORE_ITEMS`
completes a scan normally; every other enumeration/detail error or reaching
the scan cap fails closed, never returning a partial/false-unique inventory.
An unresolved matching-VID/PID candidate's open, attributes, caps or bus read
failure also aborts the inventory rather than hiding another device.
Explicitly missing bus properties retain the candidate with unavailable bus
metadata; they do not drop it. At most 64 enumerated
candidates retain evidence. Opening requires the same backend's retained
descriptor, then re-reads attributes/caps/strings on the opened handle and
re-resolves that exact collection path through SetupAPI for the same bounded
CM ancestry/property walk. Any identity, report-size, optional metadata,
devnode ancestry or raw GUID mismatch fails closed, even when both GUIDs
would classify as `Unknown`. Native read failures are preserved rather than
collapsed into `NotFound`. Composite
reopen may change only primary usage; all other checks remain.
These are post-open observations, not an atomic PnP/physical identity guarantee.
USB classification is available only when the exact native walk reaches the
USB bus; it is not a claim that collection properties yield USB in practice.

Micro/AKP03 still fail closed at the missing interface gate before transport
open/report I/O. Optional metadata does not promise successful Windows live
probing or physical qualification. Fake API-seam tests cover property
type/size/value and transient failures, bounded ancestry, incomplete scans,
exact ancestry/GUID revalidation and scoped authority.
Exact/empty/oversized descriptor tests exercise the shared injectable transport
boundary (1..=1024 bytes); they do not demonstrate Windows raw-descriptor
retrieval. No hardware or Windows runtime acceptance is claimed.

Official contracts:
[HID APIs](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/hid-api),
[HID Minidriver vs Class Driver IOCTLs](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/_hid/),
[report-descriptor IOCTL](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/hidport/ni-hidport-ioctl_hid_get_report_descriptor),
[BusTypeGuid](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/devpkey-device-bustypeguid),
[bus GUID definition contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/irp-mn-query-bus-information),
[CM_Get_DevNode_PropertyW](https://learn.microsoft.com/en-us/windows/win32/api/cfgmgr32/nf-cfgmgr32-cm_get_devnode_propertyw),
[CM_Get_Parent](https://learn.microsoft.com/en-us/windows/win32/api/cfgmgr32/nf-cfgmgr32-cm_get_parent),
[SetupDiEnumDeviceInterfaces](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupdienumdeviceinterfaces),
[SetupDiGetDeviceInterfaceDetailW](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupdigetdeviceinterfacedetailw).

## 3. Shared observation and error contracts

### 3.1 Errors and privacy

Platform error:

```text
kind: one of the values below
operation: static operation identifier
native_code: optional unsigned numeric native code
```

Kinds:

`unsupported_platform`, `unsupported_context`, `access_denied`, `process_exited`, `no_foreground`, `observation_changed`, `invalid_data`, `limit_exceeded`, `native_failure`.

Retain no native error string that might embed a path, username, device identifier, provider data, or command content. Human display is the static operation plus debug-style error kind and optional hexadecimal native code.

Windows access-denied HRESULT maps to `access_denied`; other API failures ordinarily map to `native_failure` with numeric HRESULT. Configuration-manager access denial maps similarly, and buffer-too-small maps to `limit_exceeded`.

### 3.2 Observed versus unavailable

```json
{"status":"observed","evidence":{}}
{"status":"unavailable","evidence":{"kind":"access_denied","operation":"static_operation","native_code":null}}
```

Unavailable is not a successful empty enumeration, nonexistence, released ownership, or a negative target match.

### 3.3 Record semantics

| Record | Fields and interpretation |
|---|---|
| Process identity | PID, creation time in 100-ns units, session ID; all three identify the lifetime |
| Process evidence | Identity, executable class, monotonic observation stamp |
| Process candidate | Snapshot PID/class and independent observed/unavailable lifetime inspection |
| Process inventory | Count of all examined processes; recognized candidates only |
| Foreground evidence | Numeric window handle, owner thread ID, owner process evidence |
| Desktop context | Session, optional active-console session, active/local/interactive/default-desktop booleans, stamp |
| Serial discovery | Selection state, present count, matching count, validated normalized COM names, optional selected port |

Executable classes distinguish recognized CLI, recognized native terminal host, and other. Classes are basename hints, not authenticated installation identities. A package adapter may normalize class identifiers; it must preserve these distinctions.

Do not expose full image paths, station/desktop names, raw interface paths, device-instance IDs, invalid port strings, raw SIDs, authentication LUIDs, or token IDs in reports.

### 3.4 Ordinary diagnostic report

Collect independent sections for:

1. Clock.
2. Current process.
3. Process inventory.
4. Foreground.
5. Desktop/session.
6. Serial-device discovery.

A failed section remains explicitly unavailable; unrelated successful sections remain present.

On non-Windows, all five Windows-specific sections are unavailable with `unsupported_platform` and operation `windows_platform`. Clock still uses its supported OS counter.

Limitations accompanying the report must make clear that:

- Enumeration is not terminal/device qualification.
- Ordinary diagnostics do not verify provenance, user/logon ownership, or integrity compatibility.
- Exact pane/tab identity, split-pane exclusion, and pane-to-console ownership are unproven.
- There is no UI Automation, UI text, console attachment, focus change, input, or serial opening in this ordinary diagnostic.
- Desktop/session/counter samples are snapshots, not transition subscriptions or physical timing proof.
- Readiness and qualification remain false.

## 4. Process, foreground, and desktop matching

### 4.1 Executable-name classifier

Classify a **basename**, ASCII case-insensitively:

- Native terminal host: exact expected native-host executable basename.
- Recognized CLI: exact configured CLI basename, or that complete basename followed by `.old-`, a nonempty ASCII decimal sequence, `-`, and a second nonempty ASCII decimal sequence.
- Otherwise: other.

Updater numbers may be zero or have leading zeros. Missing numbers, additional hyphens/suffixes, nondecimal digits, whitespace, path prefixes, or prefix lookalikes fail.

Recognition is independent of signature, publisher, approved-installation location, mapped-image integrity, and command line. The precise deployment basename belongs to host integration; it must not become a substring search.

### 4.2 Native Windows process inspection

For a nonzero PID:

1. Sample observation time.
2. Open a noninheritable process handle with limited-query and synchronization rights only.
3. Verify still running using a zero-time wait.
4. Read creation time, session ID, and full image name; retain only basename class.
5. Recheck liveness before returning.
6. Release process handle on return; evidence is not a live process lease.

Bounds:

- Full-image buffer: 32,768 UTF-16 units.
- Image length must be nonzero and below capacity.
- Basename must decode strictly as UTF-16.
- Creation time must be nonzero.

Errors:

- PID zero: `invalid_data` / `process_id`.
- OpenProcess invalid parameter: `process_exited`.
- Access denied: unresolved access denial, not exited.
- Signaled process handle: `process_exited` / `process_lifetime`.
- Failed wait: numeric native failure.
- Unknown wait result: invalid data.

No debug privilege, console access, or mutation rights are requested.

### 4.3 Process enumeration

Use one native process snapshot. Examine at most **16,384 entries**; a further entry is a limit error, not successful truncation.

Snapshot names must terminate and decode strictly. Only recognized name classes become candidates. Each candidate is separately inspected:

- Native access/lifetime failure is retained in its observation.
- Actual class different from snapshot class becomes `observation_changed` / `process_snapshot_identity`.
- All examined entries contribute to count, including unrecognized classes.
- Normal end is successful; other enumeration failures are explicit.

### 4.4 Pure matching

**Process match** succeeds iff expected PID and creation time are nonzero and observed identity exactly equals expected identity. It does not separately check class or freshness.

**Foreground match** succeeds iff:

- Expected window, PID, creation time are nonzero.
- Observed thread ID is nonzero.
- Observed window equals expected window.
- Owner lifetime/session exactly equals expected host identity.
- Owner class is the recognized native terminal host.

For either matcher, observed mismatch is `wrong_target`; unavailable observation is `unavailable`.

These are fragment checks, not command-readiness checks.

### 4.5 Native foreground sampling

1. Get foreground HWND; null is `no_foreground` / `GetForegroundWindow`.
2. Get nonzero owner thread/PID.
3. Hold owner process while inspecting.
4. Re-read foreground HWND and original window's owner tuple.
5. Require both unchanged; otherwise `observation_changed` / `foreground_window`.
6. Recheck process liveness.

Ordinary foreground evidence can describe any executable class. The pure matcher adds the native-host restriction. No focus/activation action occurs and no pane identity is inferred.

### 4.6 Desktop/session sampling

Sample time, determine current process session, then obtain two complete context snapshots and require equality.

Each snapshot includes:

- Active-console session ID.
- WTS connection state.
- WTS client protocol type.
- Process station is `WinSta0`, case-insensitively.
- Calling thread desktop is `Default`, case-insensitively.
- Newly opened read-only input desktop is `Default`, case-insensitively.

Do not switch desktops. Close only owned input-desktop handle, not borrowed station/thread-desktop handles.

Object names are bounded to 256 UTF-16 units; require valid even byte count, termination, strict UTF-16, and no nonzero hidden suffix. WTS scalar allocation must be nonnull and exactly requested size. Active-console UINT32_MAX becomes absent.

Before/after mismatch is `observation_changed` / `desktop_session_context`.

Pure desktop eligibility requires:

- Nonzero expected session equal observed process session.
- Active-console session equal expected session.
- Active state.
- Local protocol, numeric zero.
- Interactive station.
- Thread and input both on default desktop.

Observed failure is `unsafe_context`; unavailable is `unavailable`. Default input desktop alone does not qualify remote, inactive, service, other-session, or noninteractive contexts.

## 5. Native clocks and lifecycle invalidation

### 5.1 Clock contract

```text
ClockSample:
  source
  stamp:
    epoch
    ticks
    frequency
```

| OS | Source tag | Counter |
|---|---|---|
| Windows | `windows_qpc` | QueryPerformanceCounter / QueryPerformanceFrequency |
| Linux | `linux_monotonic` | CLOCK_MONOTONIC |
| macOS | `macos_uptime_raw` | CLOCK_UPTIME_RAW |

Other OS: `unsupported_platform` / `monotonic_clock`.

Public sampling rejects nil epoch before native work: `invalid_data` / `clock_epoch`.

Windows requires positive frequency and nonnegative signed counter.

Linux/macOS convert seconds/nanoseconds to unsigned nanosecond ticks and frequency 1,000,000,000. Seconds must be nonnegative; nanoseconds in `[0,1,000,000,000)`; multiplication/addition must not overflow. Fail rather than normalize or wrap. Native call failure is `native_failure` / `monotonic_clock` with optional numeric errno.

Source-tagged elapsed computation first rejects changed source, then checks shared stamp compatibility. Epoch/frequency/regression/overflow rules belong to the shared stamp contract.

Use system-shared native counters, not wall time or per-process origin. Comparison requires same source and boot. Source-less compatibility consumers must separately retain/check source where necessary.

**Parity edge:** low-level Windows observation sampling does not independently repeat public sampling's nil-epoch guard. A caller must enforce valid runtime epoch at its boundary rather than assuming every observation entry point does so.

### 5.2 Exact evidence age

First require successful shared stamp elapsed calculation. Epoch/frequency mismatch, zero frequency, regression, or other incompatible stamp is `clock_mismatch`.

Then reject differences of zero or one tick as `clock_mismatch`: causal ordering across threads/processes is ambiguous within one tick.

Use exact rational comparison:

```text
elapsed_ticks × 1,000,000,000
    <= maximum_age_nanoseconds × frequency
```

Use wide unsigned intermediates and saturating multiplication on the bound side. Deadline is inclusive. Do not round down duration before comparing; even a subnanosecond excess is stale. Greater age is `stale_state`.

Caller selects maximum age. This helper checks untagged stamps, not source tags or physical timing.

### 5.3 Lifecycle cuts

Lifecycle construction requires positive maximum sample gap. Initial state: fresh random epoch, no last sample, no last cut.

Cuts:

`lock`, `sleep`, `wake`, `session_changed`, `source_lost`, `clock_discontinuity`, `runtime_replaced`.

Invalidation:

1. Generate a fresh epoch.
2. Discard sample history.
3. Remember reason.
4. Require the coordinator to discard all stored old-epoch observations.

Accept a sample only if:

- Epoch equals current epoch.
- Frequency is positive.
- If previous sample exists, source/stamp elapsed calculation succeeds and gap is at most configured maximum.

Failure itself invalidates with `clock_discontinuity`; the invalid sample is not installed as the new baseline.

Sampling never clears `last_cut`, rearms physical effects, restores a target, or grants authority. A first post-cut sample has no predecessor. The separate evidence-age rule requiring more than one tick is not imposed on lifecycle observation.

This object supplies explicit invalidation, **not native notifications**. Lock/sleep/logout subscriptions and their acceptance are future integration work.

## 6. Serial-interface metadata discovery

### 6.1 Exact hardware association

ASCII case-insensitive hardware-ID grammar:

```text
USB\VID_2EC2&PID_0003
```

Optionally followed by:

- At most one `&REV_` and exactly four ASCII hex digits.
- At most one `&MI_` and exactly two ASCII hex digits.

Optional tokens may occur in either order. Reject other tokens, duplicates, extra PID, whitespace, instance suffixes, non-USB prefixes, or different VID/PID.

Match a hardware-ID value, not friendly name, interface path, instance string, or substring. VID/PID association is not authenticated hardware provenance.

### 6.2 Windows discovery

Enumerate present COM interfaces using GUID `86e0d1e0-8089-11d0-9ce4-08003e301f73`. At most **512 interfaces**; a 513th is `limit_exceeded` / `serial_interface_enumeration`.

Per interface:

1. Present means removed flag absent; active means active flag set.
2. Associate only present and active interfaces.
3. Use interface-detail size query to retrieve device-node association without retrieving interface-path text. Expected insufficient-buffer with nonzero required size is accepted; unexpected success is invalid.
4. Walk node and ancestors, stopping before root, at most **16 non-root nodes**. Detect cycles. Matching any node's hardware ID establishes association.
5. Query `PortName` in associated device's registry key with query rights only.

Hardware property:

- Buffer: **32,768 UTF-16 units**.
- Require REG_MULTI_SZ, even bounded bytes, at least two units, valid strict decoding.
- Missing property means no IDs.
- Buffer-too-small is explicit limit error.

Port property:

- Buffer: **64 UTF-16 units**.
- Require REG_SZ, even valid length, strict decoding.
- Missing value means absent.
- More-data is explicit limit error.

Native association/enumeration/property errors fail discovery rather than creating successful partial results. Normal enumeration end supplies `complete = true` to pure selection.

No serial handle is opened.

### 6.3 Port normalization and selection

Normalize ASCII case to uppercase; require `COM` plus 1–5 ASCII decimal digits, no leading zero, value 1–65535. No trimming, device-path prefix handling, Unicode digit normalization, or suffix acceptance.

Input facts: present, active, hardware-associated, optional port. These facts are not a serialized trust contract.

Matching interfaces = present AND active AND associated.

Output valid normalized ports in input order without deduplication. Selection precedence:

1. Incomplete enumeration OR not every matching interface has valid port: `incomplete`.
2. Otherwise zero matches: `not_found`.
3. One match: `unique`, select normalized port.
4. Multiple matches: `ambiguous`, even duplicate same port.

Only unique selects. Present count includes all present facts; matching count excludes absent/inactive/unrelated. Invalid raw strings are discarded, not reported.

### 6.4 Strict registry decoding

REG_SZ requires NUL terminator, strict UTF-16 before it, and zero-only suffix. Empty string may decode but is not a valid COM name.

MULTI_SZ requires at least two units and double-zero suffix. Each preceding nonempty element must be strict UTF-16. Empty element ends sequence and all remaining units must be zero. Hidden suffixes, truncation, isolated surrogates fail.

## 7. Explicit Windows qualification diagnostics

### 7.1 Requests and timing

Process request has exactly:

```text
target: process identity
requested_at: monotonic stamp
```

UI request has exactly:

```text
process: process request
window: numeric window handle
```

Unknown fields are rejected, including title, text, action, command, port, or read-text extensions.

Generic validation requires nonzero target PID/creation and age at most **12 seconds** using exact age rule. It does not independently require nonzero session.

Exact target holding validates request, opens target PID, requires full lifetime/session equality, and keeps handle during observation. Mismatch is `observation_changed` / `exact_process_lifetime`, before token/file/UI work. Native request clock/expiry failures collapse into static invalid-data operations.

Coordinator validates interval:

| Interval | Requirement |
|---|---|
| Request → helper start | Exact age ≤12s and >1 tick |
| Helper start → finish | Valid shared elapsed calculation |
| Finish → coordinator receive | Exact age ≤12s and >1 tick |
| Request → receive | Exact age ≤12s and >1 tick |

The inner execution interval does not independently require more than one tick. Total budget does not restart after slow provider or transmission.

A valid 12-second diagnostic is not action authority. Caller must check pipe correlation, supervised identity, report target/window, and lifecycle invalidation.

### 7.2 Security metadata

Hold observer and target; open tokens with query rights only.

Report:

- Same user, comparing raw SID bytes internally.
- Same logon, comparing authentication LUID internally.
- Both token contexts: session, integrity RID, elevated, UI-access, app-container.

Never report raw SIDs/LUIDs/token IDs/usernames.

Native requirements:

- Exact sizes for fixed scalar classes; size query for variable classes.
- Allocation at most **16,384 bytes**, nonzero valid returned length and alignment.
- SID pointer lies inside returned buffer; require base bytes, at most 15 subauthorities, full bounded length, native validity.
- Integrity SID has mandatory-label authority and exactly one RID.
- Token session equals held process session.
- Native boolean values exactly zero/one.
- Token ID, modified ID, authentication ID stable across snapshot.
- Reopen both primary tokens and repeat full snapshot to detect replacement as well as mutation.

Conservative eligibility requires:

- Nonzero expected session.
- Same user AND same logon.
- BOTH tokens in expected session.
- BOTH integrity exactly **0x2000**.
- Neither elevated, UI-access, nor app-container.

Low, medium-plus, high, unknown integrity fail. Same user alone fails.

General exact-process metadata **records** security results without enforcing this pure eligibility gate. UI/console helpers enforce it before their native observation.

### 7.3 Image-file evidence

This is self-image file-identity evidence, not authentication.

1. Query image paths, bounded to 32,768 UTF-16 units.
2. Accept drive-absolute form only: ASCII letter, `:`, backslash, at least four units, below capacity, no embedded NUL.
3. Require observer and target path sequences exactly equal before opening either. No case folding/canonicalization.
4. Different paths produce unavailable `unsupported_context` / `external_image_provenance_unqualified`.
5. Open equal paths for file attributes only, share-read only, existing-file disposition.
6. Require nonzero link count and nonzero file index.
7. Compare file identity by volume serial and high/low file index.
8. Requery process paths and full file-information records while held; require stability.

Success reports:

```text
same_file_as_observer: comparison result
path_file_stable_during_probe: true
provenance: unverified
```

No signature/publisher/installation allowlist, cryptographic mapped-image identity, or general path-root/reparse qualification is provided. Refusing unrelated paths narrows the probe; it does not make equal paths trusted.

### 7.4 Exact-process report

Start sample; hold exact target and current observer; independently observe security, image-file comparison, ordinary foreground, and desktop. Preserve unavailable subsection errors.

Recheck observer running; sample finish; revalidate expiry; check target running.

Output: target process evidence, observer identity, four observation sections, start/finish stamps, false readiness/qualification.

Self-target is allowed. Target/observer/expiry/liveness failure aborts entire report; subsection failure need not.

### 7.5 UI-structure helper

Windows only. Reject self-target before native UI work: `invalid_data` / `helper_must_not_be_target`.

Use one dedicated windowless MTA thread. COM-affine objects remain there. Thread creation/panic errors are static native failures. Provider hang cannot be cancelled by this function: disposable-process supervisor must kill at deadline, at most 12 seconds.

Sequence:

1. Start sample; hold target/helper; enforce conservative token context.
2. Safely convert window integer to pointer width; require nonzero top-level/root window.
3. Require nonzero owner thread/PID and target PID ownership.
4. Record foreground status; not-foreground is not itself a diagnostic failure.
5. Initialize COM MTA; create automation; get element from HWND; require returned root native HWND equals request.
6. Walk control-view tree including root depth zero.
7. At every node, validate budget and positive element PID equals target.
8. Read only control type, keyboard focus, offscreen state, process ID, and root native HWND.
9. Count visited nodes, generic text-type nodes, focused nodes, offscreen nodes.
10. Recheck window owner and target running; take final foreground sample and finish stamp; validate expiry.

Limits: **256 nodes**, depth **16 inclusive**. Attempting 257th node or depth17 fails `limit_exceeded` / `uia_tree_bound`, not successful truncation.

Native success/null means traversal end; real provider failure is not swallowed.

Report:

- Target/helper identities and requested numeric window.
- Four node counts.
- `foreground_at_both_checks`, true only when both endpoint samples were foreground.
- `relationship = "unproven"`.
- Start/finish and false qualification/readiness.

Text-control count is not pane count. No names, titles, text/value patterns, text ranges, activation, selection, focus setting, or input.

### 7.6 Console-membership helper

Windows only; reject self-target; enforce same conservative token context.

Require fresh helper without console. Nonnull existing console HWND is `invalid_data` / `helper_already_has_console`. AttachConsole also independently refuses already-attached state. Never detach an existing console to force entry.

1. Start sample and hold target/helper.
2. Attach helper to target console, owning only this attachment.
3. Read PID list with capacity **128**; zero count native failure; greater count limit error.
4. Sort ascending; reject zero/duplicates.
5. Require target and helper PIDs present.
6. Record console HWND; hold/inspect every member lifetime.
7. Reread sorted PID list and console HWND, require exact equality.
8. Recheck every member running; require exact target/helper lifetimes in list; recheck target.
9. Explicitly detach owned attachment; detach failure is error. Unwinding attempts best-effort detach.
10. Finish sample and expiry check.

Report includes sorted member identities INCLUDING helper, target/helper, optional console HWND, unproven relationship, timestamps, false flags.

Console HWND can be message-only under pseudo-console hosting and is not a terminal-pane HWND. Two equal lists are not atomic membership lease. No screen read or input opening.

## 8. Framed disposable-worker service

### 8.1 Admission

Launch only current executable with hidden argument **`__probe-worker`**, or
the fixed AKP03-only **`__akp03-worker`** route, no shell and no caller-selected executable.

Public request:

```json
{"kind":"windows_process","payload":{}}
```

Exactly `kind` and `payload`; unknown top-level fields rejected. Kind is 1–64 bytes, lowercase ASCII letters, digits, `.`, `_`, `-` only. Leading digits/punctuation are not otherwise prohibited.

Generic protocol does not interpret payload or authorize kinds. Executable must supply diagnostic-only allowlist. Typed process/console/UI kinds can be `windows_process`, `windows_console`, `windows_uia`.

Timeout **1ms through120s inclusive**. Check order:

1. Timeout.
2. Prelaunch cancellation.
3. Request validation.
4. Bounded encoding.
5. Platform dispatch.

Windows uses the Job service below. Its child issues private, thread-bound,
per-request HID authority only after validating the complete typed Micro/AKP03
options, the entire frame/EOF and allowlist, and the detached one-process,
no-breakaway, kill-on-close Job. Health, other kinds and in-process callers
receive no HID authority; native access and stale scopes are checked as in
§2.5. Optional bounded native ancestry metadata does not make Windows live
probes available: interface-number/raw-descriptor gates still fail closed.
macOS ARM64 additionally admits only health,
Micro and AKP03 requests (§8.7), validating complete typed HID options before
spawn. Other non-Windows targets return unsupported-platform after these checks
without invoking callbacks. CT remains Windows-only.

### 8.2 Exact framing

Actual transport is **anonymous inherited private standard pipes**, not a named-pipe service.

Frame:

```text
4-byte unsigned little-endian JSON body length
that many UTF-8 JSON bytes
EOF after the single frame
```

Body limit: **65,536 bytes inclusive**, excluding header.

- Zero or excessive length: `frame_limit`.
- Partial reads supported.
- Truncated header/body or I/O error: `transport`.
- Complete malformed JSON/schema: `protocol`.
- Any trailing byte, including whitespace/NUL/second frame: `protocol`.
- Encoding beyond body limit: `frame_limit`; serializer failure also collapses to that error.
- Whole envelope counts toward body size.

Request writer closes after full frame so worker can observe EOF. Response requires EOF, not merely valid payload.

Wire request:

```json
{
  "version":1,
  "request_nonce":"nonnil UUID",
  "request":{"kind":"diagnostic_kind","payload":{}}
}
```

Wire response:

```json
{
  "version":1,
  "request_nonce":"matching UUID",
  "worker":{"pid":7,"creation_time_100ns":123},
  "outcome":{"status":"ok","payload":{}}
}
```

Alternate outcome: `{"status":"callback_failed"}`.
Encoding failure outcome: `{"status":"response_encoding_failed"}`.

Encode the full response before writing any protocol stdout. If callback
serialization or the 65536-byte frame budget fails, encode this compact fallback
with the same version, nonce and worker identity, write it, then exit normally.
The supervisor maps it to `frame_limit` after pipe/exit completion. It is not
probe success or proof that no device writes occurred. No frame bound is relaxed.
Device callbacks reserve 256 bytes for maximal envelope/identity overhead, and
apply their explicit result-omission policies after cleanup.

Envelope/request/worker records reject unknown fields. Arbitrary payload does not. Do not impose blanket unknown-field rejection on outcome internals beyond actual schema behavior.

Require response version1, matching nonce, exact retained PID AND creation time. Nonce is accidental-mixup protection, not cryptographic same-user authentication.

### 8.3 Windows containment and startup

One active probe per supervisor process, acquired nonblocking:

- Concurrent active call: `busy`.
- Poisoned active lock: `supervision_blocked`.
- Sticky unresolved cleanup: `supervision_blocked`.

These fences are process-local, not durable/cross-process.

Startup:

1. Create stdin/stdout/stderr anonymous pairs; parent ends noninheritable.
2. Explicit inherited-handle allowlist contains exactly child standard handles.
3. Use current executable as explicit application; reject embedded NUL/double quote.
4. Quote executable path, append hidden-worker argument; no shell.
5. Create unnamed Job with kill-on-close and active-process-limit, limit **one**; no breakaway permission.
6. Create child **suspended**, **detached**, with extended startup/standard handles.
7. Capture creation identity from retained process handle.
8. Assign suspended child to Job.
9. Start all parent I/O threads.
10. Resume; prior suspend count must be exactly one.

Detached mode permits worker-only AttachConsole without changing coordinator's console. This is diagnostic containment, not a credential-restricted hostile-code sandbox.

Any unsupported Job condition, assignment/resume/inheritance error fails closed. Never launch unsupervised fallback. Job handle is noninheritable; its close enforces coordinator lifetime.

### 8.4 Deadline and successful completion

Native deadline begins before pipe creation/launch and includes launch, request, callback, response, EOF, stderr, exit observation. Encoding occurs before deadline begins. Poll **10ms**.

Concurrent I/O tasks:

- Write request, close stdin.
- Read one response and require EOF.
- Drain/discard stderr to EOF; limit **16,384 bytes inclusive**.

Excess stderr is `diagnostic_limit`. Never retain content. Concurrent traffic must handle payload larger than pipe buffer. Callback stdout is forbidden except protocol.

Success requires all:

- Send complete.
- Valid response and stdout EOF.
- Successful stderr EOF drain.
- Retained worker confirmed exited.
- Zero exit code.
- Version/nonce/lifetime match.
- Outcome ok.
- Parent I/O cleanup complete.

Payload receipt or child exit alone does not complete supervision.

### 8.5 Worker behavior

Before console attachment, acquire stable owned stdin/stdout/stderr duplicates:

- Each original must be native pipe type.
- Clear inheritance; duplicate noninheritable.

Read one request, require EOF, validate, acquire own lifetime identity, invoke callback once. Catch callback unwinding panic and callback error; both produce generic callback_failed with no private string or panic payload.

Encode/write one response and return. Executable must exit worker mode; service error should produce nonzero exit.

Panic hooks are not suppressed and may emit stderr, which supervisor discards within limit.

### 8.6 Failures, cancellation, and replacement

Failure kinds:

`unsupported_platform`, `invalid_timeout`, `invalid_request`, `busy`, `supervision_blocked`, `containment`, `spawn`, `protocol`, `frame_limit`, `diagnostic_limit`, `transport`, `callback`, `timed_out`, `cancelled`, `worker_failed`, `cleanup_unconfirmed`.

Display contains only static prefix `diagnostic worker: ` and debug-style kind.

Error includes optional worker lifetime identity and replacement-allowed flag:

| Situation | Replacement |
|---|---|
| Ordinary before-launch failure | Allowed; no worker |
| Busy or blocked | Not allowed |
| Launched failure, exit and I/O cleanup confirmed | Allowed |
| Exit or I/O cleanup unresolved | Not allowed; cleanup_unconfirmed overrides original |

Cancellation checked before launch, before resume, during wait. During wait, cancellation precedes deadline check. Running cancellation is not safe-to-replace until cleanup confirms all required conditions.

Cleanup:

- Terminate Job AND process when needed, covering preassignment failure.
- Wait up to **5s** for process exit.
- Set I/O-stop flag, cancel synchronous I/O, join completed threads for up to **1s**.
- Unfinished I/O retains its own buffer/handle if detached; never free resources behind a blocked OS operation.
- Unresolved process or I/O sets sticky block.
- Unwinding child ownership without certified exit also poisons supervisor.
- Wait budgets are not a physical guarantee of bounded OS/driver teardown.

Failure precedence matters:

- Framed callback_failed -> `callback`.
- Oversized cooperative callback response becomes correlated `response_encoding_failed`,
  mapped to `frame_limit`; an uncooperative/raw oversized fixture can still fail
  as `transport` or `frame_limit`.
- Stdout `not-a-frame` -> `frame_limit`, as first four bytes decode as excessive length.
- Nonzero exit -> `worker_failed` only once required pipe results/drain are available; earlier transport/protocol failures can win.

### 8.7 macOS ARM64 HID/health platform block

The existing Rust executable uses the exact hidden `__probe-worker` and
AKP03-only `__akp03-worker` routes, the same static registries, protocol version,
65536-byte frame, 65280-byte callback budget, and 16384-byte discarded-stderr
limit. Hidden routing rejects extra arguments. Parent and child both reject
unsupported kinds and malformed/unknown/missing typed options; the child also
requires one entire frame and EOF, rejects duplicate JSON keys, and rechecks
the route's allowlist before invoking any callback. No CT or Linux live route
is added.

The parent spawns only `current_exe` with `process_group(0)` and three pipes.
It verifies child PGID == PID and retains the native `proc_bsdinfo` PID/start
time. The existing `creation_time_100ns` field carries Unix-epoch process start
time on macOS, not Windows FILETIME. Response version, nonce, PID and this
native start time must all match.

Nonblocking parent I/O pumps bounded stdin, stdout and discarded stderr without
I/O threads, polling every 5 ms. Deadline includes spawn, input, callback,
response, EOF and exit observation. Micro deadlines are capture+20000 ms,
another 5000 ms for keymap; AKP03 is capture+15000 ms; health is 3000 ms.
A caller may shorten but not extend these device-spec budgets. Success requires
complete send, both output EOFs, valid correlated outcome, zero exit and cleanup.

On failure, timeout, cancellation or output overflow, reuse the Unix group
guard: WNOWAIT reserves the leader PID, signal its group with SIGKILL **before**
reaping, and allow up to 2 seconds per cleanup attempt (Drop may retry once,
up to 4 seconds total after failure). This also occurs on success to
dispose of any remaining group members. The macOS sole-zombie-group check handles
EPERM conservatively. Parent pipes are explicitly closed; no I/O tasks survive.
Unconfirmed signal/reap or ambiguous close returns `cleanup_unconfirmed`,
forbids replacement and stickily blocks the process-local supervisor. A previous
portable-child cleanup uncertainty also blocks this service.

The child checks PGID == PID and that all three standard descriptors are pipes,
then starts an independent parent-death watchdog before reading stdin and
acquires close-on-exec duplicates. The watchdog retains the parent's native
PID/start time, requires a live matching parent and a startup acknowledgement
within one second, and polls every 50 ms. Parent reparenting, zombie/exited state,
lifetime mismatch or unavailable observation causes SIGKILL of the worker's
retained process group with `_exit(1)` fallback. It does not need HID locks,
Rust destructors or cooperation from a blocked main thread. It also enforces
a 120-second maximum before full request receipt, narrowed to the validated
device-spec budget measured from watchdog startup. On ordinary completion,
it is stopped/unparked and joined using the remaining original child budget,
not a separate short shutdown grace. While waiting, the main thread continues
checking the retained parent lifetime and child deadline; either failure, a
watchdog panic or missing startup acknowledgement terminates the group. The
supervisor still enforces any shorter caller deadline, including shutdown,
EOF and exit. A delayed watchdog alone does not change a successful response
into worker failure while those deadlines remain satisfied; success still
requires zero exit and confirmed process/I/O cleanup.

Only after full request validation and watchdog startup can the child establish
a private thread-bound, per-request HID capability. Native access additionally
requires the parent process image path and current executable path to
canonicalize successfully and match. Ordinary in-process `native_backend()` calls fail with typed
`UnsupervisedAccess`; health gets no HID capability and never constructs a
backend, enumerates or opens. IOKit enumeration/open/descriptor/read and output
check the capability. A stale capability cannot revive in another request.
Numbered output retains its report ID; unnumbered output strips the synthetic
zero. Synchronous `IOHIDDeviceSetReport` has no individual write deadline:
only the supervised child's lifetime is bounded. Reopened identity is reread
from native properties; unavailable or mismatching evidence fails closed.
Stopped/finished IOKit run loops return a static protocol error; timed-out
run-loop slices return read timeout instead of spinning.

This is trusted-code liveness containment, not hostile-code/same-user
authentication, an escape-proof sandbox, proof of physical output cleanup, or
device qualification. Health reports `parent_death_watchdog=true` and 50 ms
polling, but retains `parent_death_guarantee=false`: a user-space watchdog is
not a kernel-enforced parent-death guarantee or a hard scheduling/driver bound.
It cannot run while the entire process is suspended, prove escaped-descendant
death, or overcome an uninterruptible OS/driver failure. It does not provide a
durable replacement grant or reaping proof after the supervisor itself dies;
only the live supervisor's normal process/I/O cleanup can grant replacement.
Self-authored tests observe orphaned blocked workers exiting and being reaped
after SIGINT/SIGKILL of the parent, and exercise held pipes and truncated
response headers/bodies. No hardware was accessed.
IOKit property/descriptor and physical input/output behavior remain unverified.
Standalone doctor discovery remains unavailable; diagnostic metadata is obtained
only inside the explicit HID probe child. Windows uses the separate Job
service, not this process-group block. Its HID backend now requires the
private validated live-child scope described in §2.5/§8.1, retains bounded
native bus ancestry and rejects incomplete enumeration. The underlying Job
lifetime/cleanup service remains as specified above; Windows interface numbers
and original report descriptors remain unavailable, so live probes fail
closed before transport open/report I/O. Windows runtime/physical acceptance
is still unverified.

## 9. Portable bounded stdio diagnostics

Separate from the framed worker service. Internal caller supplies a Command; this does not expose arbitrary process launch to external request payloads.

Common:

- Windows/Linux/macOS supported.
- Timeout positive and at most120s; positive submillisecond is not separately rejected.
- Null stdin, captured stdout, null/discarded stderr.
- Stdout maximum **262,144 bytes**; next byte -> `output_limit`.
- Successful exit plus completed transport required to return bytes.
- Errors: invalid_timeout, unsupported, spawn, transport, failed, output_limit, timed_out, cleanup_unconfirmed.
- Static display prefix `diagnostic child: `.
- Sticky process-local unresolved cleanup blocks later captures.

### 9.1 Linux/macOS

Spawn new process group with child leader. Stdout nonblocking; drain at most16 chunks of8,192 bytes per loop, handle interruption, sleep5ms.

Observe leader using wait-without-reaping. Do not reap/reuse leader PID until group signaled. Finish sends SIGKILL to negative leader PID even after leader exited, to kill pipe-holding descendants. ESRCH acceptable.

macOS-only EPERM exception: accept only when exited retained leader is confirmed sole zombie group member; generic EPERM is not acceptable.

Wait up to2s for leader exit, poll5ms, reap. Preserve already-successful leader status while killing remaining group. Success still requires stdout EOF.

Cleanup failure may override earlier error. Drop retries unfinished cleanup and blocks on unresolved result.

Process groups are not sandbox, parent-death containment, or proof of escaped descendant death. Do not use for action-writer quiescence.

### 9.2 Windows direct child

Ordinary spawn, no suspended Job. One reader reads at most max+1 bytes, reports via bounded channel; coordinator polls10ms.

After child exit, reader must finish by deadline. A descendant holding pipe can cause cleanup_unconfirmed. Kill direct child if needed and wait. Give unfinished reader one extra second; unresolved reader/child sets sticky block and overrides result.

This is direct-child cleanup, not descendant containment.

## 10. Cooperative runtime ownership

Use one approved authority directory, independent of configuration root. All participants in same authority domain must agree on same anchor.

Acquisition:

1. Open authority through shared safe-root primitive; failure `unsafe_root`.
2. Open fixed `runtime-owner.lock`; failure `unavailable`.
3. Require file metadata readable and file length zero; nonzero -> `ownership_changed`.
4. Nonblocking exclusive lock; would-block -> `busy`, other failure -> `unavailable`.
5. Validate root/held-file/name association through safe-root lock validator; failure `ownership_changed`.
6. Hold root/file; generate fresh random runtime epoch.

The Rust `RuntimeOwnerLease` API receives the approved authority root
explicitly. It does not derive that root from a configuration root, create a
missing anchor, write owner data, or infer a dead owner.

Every epoch read rechecks zero length and root/held-file/name association before returning same epoch. Metadata failure unavailable; size/association failure ownership_changed.

No owner data is written, no PID-liveness test, no force-unlock, no takeover, no durable fencing generation, no journal, no termination of competing runtime.

A free lock does not prove dead action writers or safe replacement. Dropping owner releases resources, not authority to replace live writers.

Error display: static `runtime ownership refused: ` plus debug-style kind; generic I/O conversion does not add native path data.

## 11. Backend identities and unresolved run outcomes

### 11.1 Opaque ID

- UTF-8 string byte length **1–256**.
- Reject any Unicode control character.
- No trimming, normalization, case folding, UUID requirement, or other character restriction.
- Spaces/punctuation/noncontrol Unicode allowed.
- JSON deserialization uses same validation.

### 11.2 Neutral target schema

Backend names below are normalized package terminology. Preserve functional distinction and identity fields even if an external adapter has a different spelling.

```text
Native Windows target:
  backend: windows_native
  process: process identity
  window: numeric handle

Integrated-host target:
  backend: integrated_host
  host_epoch: runtime epoch
  target: opaque ID
  attachment: opaque ID
```

Reject unknown fields. Correlation is not authentication. Wrapper alone does not add explicit nonzero-window/nonnil-epoch/lifetime checks; nested shared-contract validation still applies.

Registration wraps an opaque ID but is deliberately not target identity or grant.

### 11.3 Run scope and delivery result

```text
RunScope:
  target
  provider_epoch
  run: UUID

ScopedOutcome:
  scope
  intent: UUID
  outcome: not_admitted | completed | unknown
```

Reject unknown fields.

`blocks_scope(candidate)` iff stored outcome is unknown AND complete stored scope equals candidate. Intent is not part of scope equality.

Changing backend, host epoch, attachment, target, provider epoch, run, or native lifetime/window makes another scope—not resolution of original unknown. Querying original still blocks. Not-admitted/completed do not block by this helper.

Timeouts, reconnect, or backend switching never turn unknown into completion or permission to replay. This is in-memory correlation for future durable journal, not implemented reconciliation/admission/retry.

## 12. Acceptance vectors

### 12.1 Identity and context

1. Process `(123,456,3)` matches identical observed lifetime. Change PID, creation, or session individually -> wrong_target.
2. Expected PID0 or creation0, even against matching invalid observation -> wrong_target.
3. Unavailable access-denied/exited/changed/native-failure -> unavailable, never free ownership.
4. Window42/thread50/exact host identity/native-host class -> foreground match. Window99/thread0/reused creation/different session/other class -> wrong_target.
5. Missing foreground -> unavailable.
6. Exact CLI basename, uppercase version, updater `.old-123-456` and case-varied `.OLD-0-9` -> CLI class. Prefix lookalike, `.old-1-2-extra`, missing number, nondigit, trailing space, full path -> other.
7. Desktop session3/console3/all flags true -> success. Flip any flag, missing/different console, different process session, session0 -> unsafe_context.
8. Both tokens session3, medium0x2000, same user/logon, no special flags -> success. For either token, session0/4, integrity0/0x1000/0x2100/0x3000, elevation/UI-access/app-container -> unsafe_context.

### 12.2 Native process and privacy

1. Repeated Windows self inspection -> same nonzero lifetime; serialized evidence no full path/current directory.
2. PID0 -> invalid_data.
3. Changed expected creation/session fails exact probe before token/file/UI work.
4. Self metadata may show same-user/logon and same image, but provenance unverified and flags false.
5. Report has no SID/LUID/token ID/path/username strings.
6. UI/console helper self-target fails before native UI/console calls.

### 12.3 Time and lifecycle

1. Epoch1, frequency10,000,000, ticks100→200, max3s -> pass.
2. Now99/100/101 -> clock_mismatch; changed epoch/frequency or zero frequency -> clock_mismatch.
3. Frequency2,000,000,000, ticks0→6,000,000,000, max3s -> pass; one extra tick -> stale despite half-nanosecond excess.
4. FrequencyUINT64_MAX, ticks0→UINT64_MAX, max1s -> pass without overflow.
5. Native `(seconds,nanos)` = `(-1,0)`, `(0,-1)`, `(0,1_000_000_000)`, `(INT64_MAX,0)` -> reject; `(1,20)` ->1,000,000,020 ticks.
6. Nil epoch public clock -> reject.
7. Same-epoch child counter sample lies between parent's before/after samples on supported OS. This tests shared counter origin, not physical timing.
8. Probe frequency10: request100/start110/finish120/receive130 -> pass.
9. Start100/101, finish109 before110, receive121/119 with finish120, epoch/frequency mismatch -> clock_mismatch.
10. request100/start110/finish200/receive220 -> exact12s pass; receive221 -> stale.
11. Lifecycle gap3s/frequency10/ticks10→40 -> boundary pass;41 -> cut/new epoch.
12. Source change, regression, zero/changed frequency, wrong epoch -> cut/new epoch.
13. Explicit lock/sleep/wake/session/source/runtime cut changes epoch; valid next sample does not clear cut.
14. Zero maximum lifecycle gap -> reject.

### 12.4 Serial metadata and decoding

1. Exact USB ID, case variation, REV_0001, MI_00, both in either order -> match.
2. PID_00030, VID_2EC20, PID_0004, HID prefix, instance suffix, extra PID, MI_ZZ, duplicate MI, REV_1, interface-path syntax -> reject.
3. COM3/com17/COM65535 normalize successfully.
4. COM0/COM03/COM65536/COM+3/COM3extra/spaces/COM alone/device-path prefix/full-width digit -> reject.
5. One present-active-associated COM17 plus absent/inactive/unrelated facts -> unique COM17.
6. Only excluded facts -> not_found.
7. Two valid facts or duplicate COM3 twice -> ambiguous, no selection.
8. Missing/invalid port or incomplete enumeration -> incomplete; invalid raw path not serialized.
9. REG_SZ `[67,79,77,51,0]` -> COM3; missing NUL, isolated surrogate, `[67,0,80,0]` -> reject.
10. MULTI_SZ `[65,0,66,0,0]` -> A/B; `[0,0]` -> empty; truncated, hidden suffix, invalid surrogate, `[0]`, empty -> reject.

### 12.5 Framing and admission

1. Valid frame read one byte at a time round-trips; final EOF succeeds.
2. Length0/65,537/UINT32_MAX -> frame_limit.
3. Two-byte header, or length4 with only `{` -> transport.
4. Complete one-byte `{` body -> protocol.
5. Valid frame plus NUL/whitespace -> protocol on EOF check.
6. 65,536-character JSON string plus framing envelope -> frame_limit.
7. Wrong version/nil nonce/empty kind/uppercase/slash/space/65 bytes -> invalid_request.
8. Kind `health --command` -> no launch, safe replacement.
9. Extra executable field -> deserialize reject.
10. Response version/nonce/PID/creation changed independently -> protocol.
11. Public timeout0/1ns/>120s -> invalid_timeout/no worker/replacement allowed.
12. Pre-set cancellation -> cancelled/no worker/replacement allowed.

### 12.6 Controlled Windows worker

Use test-only callbacks, never expand production allowlist for fixtures:

1. Health reports detached, Job limit1, kill-on-close/active-limit, no breakaway; false readiness. Return only after exact worker lifetime exited.
2. Unrelated parent inheritable event is not inherited; child cannot signal it using numeric handle value.
3. Echo48KiB string -> no pipe-buffer deadlock.
4. Stall60s with750ms deadline -> timed_out, retained worker identity, confirmed exit, replacement allowed; controlled elapsed under10s; next health succeeds.
5. Cancel stalled probe after750ms -> confirmed exit before cancelled safe-replacement result.
6. Private callback error -> generic callback, no private text.
7. Cooperative callback returning a 128KiB value -> correlated
   `response_encoding_failed` fallback, mapped to `frame_limit` after confirmed
   worker exit; replacement allowed. This is not an uncooperative raw-frame case.
8. Stdout `not-a-frame` -> frame_limit.
9. Stderr32KiB -> diagnostic_limit.
10. Each fully cleaned failure permits subsequent health.
11. Five further health calls do not grow controlled parent's native handle count by more than one.
12. Unconfirmed exit or I/O cleanup -> cleanup_unconfirmed, no replacement, all later probes blocked.

### 12.7 Portable children, ownership, correlation

1. Success marker captured; exit7 -> failed;262,145 bytes -> output_limit.
2. Stall30s with100ms budget -> timed_out; controlled elapsed under4s.
3. Unix shell starts background30s sleeper, prints `done`, exits0 -> exactly `done` under4s after group cleanup.
4. Unsafe authority root -> unsafe_root; nonempty lock -> ownership_changed; valid contended anchor -> busy.
5. Lock size/name association changes after acquisition -> epoch read fails, no silent replacement authority.
6. Opaque target serializes without synthetic PID.
7. Empty/257-byte/control-containing opaque ID -> reject;256-byte noncontrol -> accept.
8. Unknown exact scope blocks. Change backend/host epoch/attachment/provider epoch/run -> different scope not matched, original still blocked.
9. Extra registration field in target -> reject.

## 13. Required integration and non-goals

Connect these implemented interfaces explicitly:

- Shared epoch/process/stamp validation and serialization.
- Hidden-worker executable routing and diagnostic-only kind allowlist.
- Typed request/report JSON conversion.
- Pending-request/report target, window, helper identity, and interval validation.
- Explicit lifecycle cuts and invalidation of all old observations.
- Shared safe-root/lock primitives and single per-user authority anchor.
- Maintenance-capability import, without promoting it to physical qualification.

Native lifecycle notification sources, host authentication/admission,
host-confirmed presentation, device physical qualification, a durable
unknown-outcome journal, all-writer quiescence, safe action recovery, and
aggregate action readiness remain unavailable. They are future integration
gates, not additional implementation requirements for this reconstruction.

Do not replace missing pieces with positive diagnostics. In particular:

- Native lifecycle acceptance is absent.
- Exact pane/tab/console ownership and split-pane exclusion are unproven.
- External executable provenance is unqualified.
- Process-local blocking is not durable recovery fencing.
- Process groups and direct-child capture are not action containment.
- Opaque IDs are not host authentication.
- No terminal action becomes enabled by implementing this appendix.
