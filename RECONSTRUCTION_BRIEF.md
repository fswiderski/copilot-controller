# Rust controller reconstruction brief

## 1. Start here

Build a new Rust package named `copilot-controller`, with an executable of the
same name and a reusable library. Implement the behavior described in this brief
and its three local appendices. The implementation must not require another
repository, its source files, its scripts, its assets, or its documentation.

This is a behavioral handoff, not permission to copy an implementation. Write new
code, tests, documentation, and artwork. Do not copy firmware images into the
repository. Firmware maintenance accepts separately supplied, explicitly pinned
local artifacts.

### Reading order

1. This document: scope, shared engine, presentation, persistence, implementation
   sequence, and acceptance plan.
2. [Display devices](specs/DISPLAY_DEVICES.md): two screen-equipped device
   families, their protocols, input, rendering, and diagnostic probes.
3. [Micro and firmware](specs/MICRO_AND_FIRMWARE.md): HID framing, RPC, input,
   health, LEDs, and the optional firmware-maintenance path.
4. [Platform and diagnostics](specs/PLATFORM_AND_DIAGNOSTICS.md): clocks, process
   identity, observation, qualification, ownership, and worker supervision.

The appendices are part of the specification, not optional background reading.
Their protocol constants, timing limits, validation rules, and test cases are
required for parity.

### Meaning of parity

Reproduce the implemented functionality, including refusals, default-disabled
behavior, bounded diagnostic experiments, and explicit unavailable results.
Do not implement planned capabilities merely because a type or policy describes
them.

Package names, provenance strings, generic host names, and original artwork may
be newly named. The rebuilt product uses `integrated` for its managed host choice
and `standalone` for its default host choice. This is functional parity, not a
requirement for identical branding, copyrightable artwork, internal APIs, JSON
object ordering, random UUIDs, timestamps, or environment-specific observations.
Hardware wire bytes, state transitions, safety decisions, data validation,
transaction behavior, geometry, and documented interface shapes must match.

Do not claim pixel-identical artwork or hardware acceptance from offline tests.
When a function is unavailable, return that fact and a reason; do not turn
unavailability into a successful empty result.

## 2. Product boundary

The product is a diagnostic foundation and offline six-session controller
engine, not a complete interactive terminal controller.

| Area | Implement | Do not infer or add |
|---|---|---|
| Devices | Micro HID; AKP03 display HID; CT serial/display protocols | A plugin marketplace or automatic firmware selection |
| Session engine | Strict preferences/bindings, event reduction, event tailing, hook inbox, fork-follow bookkeeping | A complete running session-discovery service |
| Presentation | Six slots, shared colors, status words, selection, effect negotiation, synthetic previews | Live device authority from previews |
| Input | Decode device reports; normalize discrete events; validate candidate roles and clean gestures | Keyboard injection or terminal commands |
| Policy | Structural action guards and process-local duplicate suppression | Authenticated host grants or a durable action journal |
| Platform | Read-only observations, clocks, safe state roots, cooperative ownership, bounded workers | Completed native lock/sleep/logout subscriptions |
| Maintenance | Explicit pinned-artifact Micro USB maintenance on one platform | Bluetooth flashing; update paths for the other devices |

There must be no public `run`, `send`, `accept`, `stop`, `fork`, or `_control`
command. Every inventory and preview that reports action readiness reports
`command_ready: false`. A successful action-policy check is not a capability to
send input.

The ordinary `doctor` command does not open controller devices, initialize them,
write reports, update firmware, or deliver input. Device discovery requires a
device-specific explicit option. Opening a device requires a bounded probe.
Mutating a display, LED state, or firmware requires additional explicit opt-in.

### Supported build matrix

Use Rust edition 2024 and toolchain `1.99.1` as the initial reproducible baseline.
Commit the toolchain selection and lockfile, and support:

| Capability | Windows | macOS | Linux |
|---|---|---|---|
| Pure engine, encoders, decoders, JSON/SVG previews | Yes | Yes | Yes |
| Native observations | Platform-specific, with explicit limitations | Platform-specific, with explicit limitations | Platform-specific, with explicit limitations |
| Micro and AKP03 HID metadata/probes | Optional worker-only metadata; live probes fail closed on missing exact evidence | Optional feature | Unavailable, even when feature selected |
| CT serial probe | Native implementation | Bounded diagnostic, ARM64 only | Unavailable |
| Micro firmware update | Unavailable | Optional feature, ARM64 only | Unavailable |
| Safe persistent state and cooperative owner | Fixed local disk | Local APFS | Local ext-family, XFS, Btrfs |

Use three optional Cargo features: `akp03-hid`, `micro-hid`, and
`micro-firmware`. They are independent: `micro-firmware` enables offline
pinned-artifact validation, not HID metadata or probing; select `micro-hid`
separately for those capabilities. This keeps local artifact validation from
implicitly enabling a native device backend. Pure engine, codec, and preview
behavior remains available without these features. Native firmware execution
remains unavailable. Building a feature does not imply a connected device,
permissions, qualification, enrollment, or terminal-action readiness; native
probes still require explicit opt-in, worker containment, and exact evidence.

## 3. Observable command interface

Use kebab-case subcommands. Produce pretty-printed JSON and a trailing newline
unless the command explicitly produces SVG. Write diagnostic errors to stderr
and return a nonzero exit status. Argument-validation errors must occur before
device access or worker launch. A diagnostic report that says the probe or
update failed must not exit successfully just because its JSON serialized.

| Command | Required behavior |
|---|---|
| `--help`, `--version` | Normal CLI help/version; describe the read-only boundary; hide worker commands |
| `capabilities` | Offline OS/host/device/maintenance/action inventory; no discovery or permission request |
| `device-profiles` | Three profiles with descriptors, controls, prioritized roles, evidence, and blockers |
| `device-preview <micro\|ajazz\|ct>` | Six synthetic realized slots as JSON |
| `device-preview ... --reduced-motion --high-contrast --brightness <n>` | Respect preferences and explicitly report unsupported fallbacks; brightness defaults to `0.25` |
| `device-preview ajazz --svg` | Upright logical SVG; reject `--svg` on the other devices |
| `device-assign` | Read one bounded recorded gesture from stdin; return candidate logical binding, never activate it |
| `device-assign --export-root <absolute-dir> --confirm ASSIGN` | Exclusively create `device-binding.json` in an approved existing private directory |
| `doctor` | JSON observation report with schema, read-only flag, platform probes, device capabilities, inventory, and limitations |
| `micro-doctor [--discover]` | Offline profile/capabilities, or explicit metadata-only HID enumeration |
| `micro-decode <hex-report>...` | Offline decode of 1 through 128 reports; never open HID |
| `micro-preview` | Synthetic six-LED diagnostic frame, no device writes |
| `micro-probe ...` | Bounded supervised passive capture; queries and LED test separately opted in |
| `micro-firmware-plan ...` | Offline validation of supplied pinned local image/tool artifacts |
| `micro-firmware-update ...` | Explicit confirmed, supervised USB maintenance; see appendix |
| `akp03-doctor [--discover]` | Offline capabilities/profile, or metadata-only enumeration |
| `akp03-decode <hex> [--rev2]` | Original-profile decoding by default; explicit rev2 decoding when selected |
| `akp03-preview` | Synthetic original-profile six-slot SVG |
| `akp03-probe ...` | Bounded supervised rev2 passive capture; initialization/display writes separately opted in |
| `probe-health` | Contained worker round trip without device access |
| `ct-probe [--capture-ms <0..5000>] [--test-pattern]` | Isolated bounded CT handshake/query/capture; default capture 2000 ms |

The appendices specify flattened probe/maintenance options, exact bounds,
output records, hidden worker protocols, and platform errors. Public host
names in reports are `integrated` and `default_terminal`; they describe different
capability surfaces, not evidence that either is action-ready.

`doctor` succeeds if it produces its report even when individual observations
fail. It includes `schema: 1`, `read_only: true`, `command_ready: false`, and
`qualification: "foundation_only"`. Probe success means the bounded experiment
completed, not that physical input, visible output, or host control is qualified.

The integration package currently routes offline inventories, synthetic previews,
bounded recorded assignments, decoders, and pinned local firmware-plan validation.
`doctor` samples the native clock and available read-only process/desktop
observations; serial/device discovery is deliberately unavailable. Contained
health workers run on Windows/macOS ARM64. Explicit `ct-probe`, `micro-probe`, and
`akp03-probe` routes reuse the same-executable bounded Windows diagnostic
supervisor, with static kind-only HID authority and validated options/results.
Micro/AKP03 `--health-check` uses pure no-I/O reports on other platforms and
requires no HID feature. Live HID probing requires the device-specific feature
and native descriptor evidence. Windows exposes optional BusTypeGuid metadata
through a bounded native parent walk across exact logical HID bus nodes
(only a documented USB bus GUID is classified; missing/unknown transport stops
the walk). Incomplete enumeration or unresolved matching candidates fail closed.
Exact interface numbers and original report descriptors remain unavailable, so live probes
fail closed. Windows HID access requires a private, scoped contained-child
capability; there is no in-process fallback. See platform appendix §2.5 for
the supported API contract and residual evidence gaps.
Micro/AKP03 also use the macOS ARM64 bounded process-group worker,
with the exact hidden routes, framing and device-spec budgets. All IOKit access
and writes require a private, scoped child capability and same-executable parent;
ordinary public in-process native access fails closed. Health constructs no HID
backend. macOS native evidence remains unverified on hardware, and process groups
use a child-side parent-lifetime watchdog, not a kernel-enforced parent-death or
escape guarantee; whole-process suspension and OS/driver stalls remain outside
its guarantee. CT also uses the macOS ARM64 worker for an explicitly invoked,
bounded serial diagnostic; the worker alone may discover and open the uniquely
associated USB serial endpoint. Linux live probing remains unavailable. See
platform appendix §8.7 for cleanup/fencing.
State snapshots/recovery and shared
authority-anchor ownership are explicit library-only APIs; no CLI route invokes
them. No native updater is wired. No probe is action-ready or physically
qualified; cross-compilation and synthetic tests are not hardware acceptance.
HID results are budgeted after native cleanup. Oversize exports fail explicitly
with omission/truncation metadata, keeping whole semantic values or whole report
prefixes, native write counts and cleanup flags. The unchanged bounded worker
frame has a correlated compact encoding-failure fallback; it no longer silently
loses oversized callback responses after possible writes.
Windows namespace mutations remain explicitly unavailable. Compiled codecs and
test-only maintenance models are not runtime or physical acceptance.
Native persistence acceptance tests require an explicitly approved filesystem
and are ignored by default; Unix CI executes them on its approved roots.

## 4. Common types and time rules

### Identity

- Slots are integers 1 through 6, with the same validation for direct
  construction and deserialization. Reject zero, seven, negatives, floats,
  numeric strings, null, and integer overflow.
- Session identity is a UUID. Wherever canonical text is required, require
  the 36-character hyphenated representation; allow uppercase hex, normalize on
  output. Do not accept braced or unhyphenated spellings.
- A runtime epoch is a UUID, created afresh for a new runtime/lifecycle epoch.
- The legacy native process identity is `(pid: u32, creation_time_100ns: u64,
  session_id: u32)`. This is a Windows identity; never invent Windows fields for
  Unix or for an opaque integrated-host target.
- Five generation counters track connection, profile, selection, binding, and
  presentation independently. All participate in structural intent checks.
- Opaque targets and native targets are tagged alternatives, not convertible
  merely because they share a numeric handle.

### Time

A monotonic stamp contains `{epoch, ticks: u64, frequency: u64}`. A platform
sample also names its clock source. Elapsed-time comparison fails when the
source changes, epoch differs, frequency is zero or changes, or ticks regress.
Compute whole seconds plus remainder-derived nanoseconds using widened
arithmetic; large counters must not overflow.

Examples:

- ticks 10 to 25 at 10 Hz = 1.5 seconds.
- `u64::MAX` ticks at 1 Hz is representable as that many seconds.
- `u64::MAX - 1` ticks at frequency `u64::MAX` truncates to 999,999,999 ns.

Source event timestamps order event history; they are not current action
freshness. Wall time is used only where explicitly specified, such as a hook
heartbeat or pending-fork retention. Do not substitute wall time for monotonic
eligibility or coalescing.

### Enum vocabulary

Statuses: `off`, `idle`, `working`, `complete`, `waiting`, `error`, `unknown`.

Actions: `none`, `autopilot`, `accept`, `stop`, `fork`, `send`, `model_picker`,
`up`, `right`, `down`, `left`.

Refusal reasons: `disabled`, `unsupported`, `unknown_ui`, `stale_state`,
`clock_mismatch`, `wrong_target`, `changed_binding`, `changed_generation`,
`changed_run`, `changed_draft`, `changed_dialog`, `command_draft`,
`empty_draft`, `busy`, `broad_approval`, `duplicate`, `indeterminate_delivery`,
`pending_input`, `unavailable`, `unsafe_context`, `expired_intent`.

Preserve distinctions: unknown is not off; unavailable is not empty; a pending
selection is not a confirmed selection; a refusal is not delivery failure.

## 5. Strict configuration

### 5.1 Parsing and bounds

Standalone settings, binding, and slot-map inputs are at most 65,536 bytes,
including whitespace. Check size before parsing. Reject malformed JSON, trailing
garbage, unsupported versions, unknown schema fields, duplicate declared fields,
and duplicate map keys. Do not silently drop fields or recover by defaulting.
Strictness also applies to nested typed records.

Optional binding fields may be absent, but if present must have the proper
non-null type. Defaults apply to omitted preference fields, not invalid values.
Booleans are booleans, not strings or numeric flags.

### 5.2 Settings schema and defaults

| Field | Type / default / rule |
|---|---|
| `version` | Required integer `1` |
| `brightness` | Finite number in `[0,1]`; default `0.25` |
| `voice_enabled` | Boolean, false |
| `controls_enabled` | Boolean, false |
| `auto_bind` | Boolean, false |
| `title_probe` | Boolean, false |
| `display_titles` | Boolean, false; independent consent from probing |
| `window_switcher` | Boolean, false |
| `launch_host` | `standalone` by default, or `integrated` |
| `bindings` | Optional partial override map over the allowlist below |
| `dpad_angles` | Optional partial direction-to-angle map |

Defaults:

| Binding key | Action |
|---|---|
| `ACT06` | `autopilot` |
| `ACT07` | `accept` |
| `ACT08` | `stop` |
| `ACT09` | `fork` |
| `ACT12` | `send` |
| `DPAD_UP` | `up` |
| `DPAD_RIGHT` | `right` |
| `DPAD_DOWN` | `down` |
| `DPAD_LEFT` | `left` |

Only these nine keys may be overridden. A DPAD binding may be a direction or
`none`, never a command. An omitted key retains its default mapping.

An explicit recommended preset sets brightness to `0.75`, enables controls,
auto-bind, title probing, and window switching, and leaves voice and displayed
titles disabled. It is not the loading or recovery default.

Calibration keys are exactly `up`, `right`, `down`, `left`; angles are finite
turns in `[0,1)`. Partial calibration is accepted. With all four present, compare
every pair in the order up/right/down/left: for difference `a-b`, circular
distance is `abs(((a-b+0.5) mod 1)-0.5)`, with positive modulo implemented by
subtracting floor. Refuse any distance below `0.15`; preserve floating-point
evaluation order at the boundary.

### 5.3 Binding schema

Required: `session_id` canonical UUID text, `terminal_title` nonempty text of
at most 512 Unicode scalar values with no control characters.

Optional/defaulted:

| Field | Rule |
|---|---|
| `pinned` | Boolean, false |
| `custom_label` | At most 80 Unicode scalars; see label rules |
| `pid` | Positive u32 |
| `process_created` | Nonempty, at most 128 scalars, no controls |
| `session_root` | Lexically valid absolute Windows path, at most 4096 scalars |
| `label` | Nonempty, at most 512 scalars, no controls |
| `title_prefix` | Boolean, false |
| `initial_idle_confirmed` | Boolean, false |
| `assignment` | `automatic` or `explicit` |
| `target` | Strict terminal-anchor object described below |
| `fork_parent` | UUID |

An anchor contains `handle: u64 > 0`, `host_pid: u32 > 0`,
`host_created` nonempty/no controls/at most 128 scalars, and `runtime_id`.
The runtime ID is 2 through 32 comma-separated signed decimal i32 components,
with no whitespace or plus signs. These are structural checks, not proof of
the current process, pane, or ownership.

Session-root validation accepts drive-rooted paths such as `C:\work` and UNC
paths with server/share. Reject device namespaces `\\?\` and `\\.\`, controls,
relative paths, characters `< > " | ? *`, component-ending dots/spaces, and
colons except the drive separator. This compatibility-data field stays
Windows-shaped even when the storage engine runs on Unix; it is not the native
state-root contract.

Label validation uses Unicode 16.0 general categories. Permit multilingual
letters, combining marks, symbols, and ordinary separators. Reject every
character in categories Control, Format, Surrogate, PrivateUse, or Unassigned.
Count scalar values, not bytes or grapheme clusters. Empty custom labels are
accepted as input; the label setter treats empty as clearing the override.

Presentation label precedence:

1. A custom label, including an explicitly supplied empty one.
2. Only when `display_titles` is true: `label` if supplied, else terminal title.
3. Otherwise `Slot N`, regardless of permission to probe titles.

### 5.4 Slot map and mutations

Shape: `{"version":1,"revision":0,"slots":{"1":{...binding...}}}`.
`revision` defaults to zero. Keys are exactly `"1"` through `"6"`, never `"01"`.
At most six bindings; UUID identity must be unique across slots, including
case-equivalent UUID spellings.

All mutation methods validate an expected revision, increment without wrapping,
and leave the entire object unchanged on failure. Refuse revision exhaustion.

- Refreshing the same session in a slot preserves pin and custom label.
- Replacing a different session is allowed only if the existing binding is not
  pinned and supplied health is `process_exited` or `pid_reused`.
- `live` and `unresolved` health never make a bound slot reusable.
- If an expected current session is supplied, require it still matches.
- Refuse a session already present in another slot.
- Explicit unbind requires slot, expected session, and expected revision.
- Pin/label setters require an occupied slot and current revision.
- Capacity never causes eviction.

Auto-discovery is an API over supplied candidate data, not a background scanner.
Require `auto_bind`. If the session is already bound, return its existing slot
without changing it. Otherwise choose the first numerically available empty or
reusable slot. A new automatic binding clears custom label, pin, and initial-idle
confirmation and sets assignment to automatic.

## 6. Event ingestion and state reduction

### 6.1 Minimized event records

An event input is at most 262,144 bytes and contains `type`, `timestamp`, and
optional `data`. Consume unknown ingress fields without retaining their values.
Never persist conversation bodies, tool arguments, notification text, directory
names, raw partially read records, or those values in errors/debug output.

Map event types:

| Wire type | Structural event |
|---|---|
| `user.message` | UserMessage |
| `assistant.turn_start` | AssistantTurnStart |
| `assistant.message` | AssistantMessage with content/tool-request presence only |
| `assistant.turn_end` | AssistantTurnEnd |
| `session.task_complete` | TaskComplete |
| `abort` | Abort |
| `session.error` | SessionError |
| `tool.execution_start` | ToolExecutionStart; `asks_user` only if `toolName == "ask_user"` |
| `tool.execution_complete` | ToolExecutionComplete |
| Anything else | Unknown, without inventing a transition |

For `content` and `toolRequests`, presence has value semantics: null, false,
zero, empty string, empty array, and empty object are absent. Nonzero numbers,
true, nonempty strings, and nonempty containers are present. Do not retain the
contents.

Timestamp input:

- Unsigned integer milliseconds; nonnegative signed integers also work.
- Finite floating values from zero through `2^53-1`, truncated to milliseconds.
- ASCII ISO timestamps, 20 through 35 bytes, with `T`, seconds, and mandatory
  `Z` or signed `HH:MM` offset.
- Years 1970 through 9999; real calendar/leap-day validation; no leap seconds.
  Hours <=23 and minutes/seconds <=59; offset hours <=23/minutes <=59.
- Fractional seconds contain 1 through 9 digits; take up to the first three and
  right-pad to milliseconds. Reject resulting dates before the Unix epoch.

Known answer: `2026-10-14T10:00:00.123Z` and
`2026-10-14T12:30:00.123456+02:30` both equal `1791972000123`.

### 6.2 State machine

Initial state: Idle, event timestamp 0, no final candidate, last hook timestamp 0,
no pending kind/time, no run token.

Ignore any event whose timestamp is strictly below the current meaningful-event
timestamp. Equal timestamps retain arrival order.

| Event | Transition / side effect |
|---|---|
| UserMessage, AssistantTurnStart | Working; clear final candidate |
| AssistantMessage | Set final candidate to content-present AND no tool requests; advance meaningful timestamp, do not change status |
| AssistantTurnEnd | Complete only if final candidate is true |
| TaskComplete, AgentStop hook | Complete |
| Abort | Idle; clear final candidate |
| SessionError, ErrorOccurred hook | Error |
| ToolExecutionStart asking user | Waiting; pending kind ElicitationDialog |
| Other ToolExecutionStart, UserPromptSubmitted hook | Working |
| ToolExecutionComplete | Working only if currently Waiting |
| SessionStart hook | Idle |
| SessionEnd hook | Off |
| Notification hook with recognized pending kind | Waiting; set that kind |
| PostToolUse hook | Working unless already Complete |
| Other event | No status/timestamp transition |

For any accepted hook, update `last_hook_ms` to max(old, hook time), even if the
hook makes no state transition. Unknown/nontransitioning events do not advance
the meaningful-event timestamp. Consequently, a newer unknown event must not
hide a delayed meaningful event.

On entering Working, create `run_token = event timestamp` when the old state is
neither Working nor Waiting, or when the event is a new user message/prompt.
Tool turns and waiting/resumption remain in the same run.

Entering Waiting sets `pending_at_ms`. Any other actual transition clears
`pending_kind` but does not erase the old pending timestamp. Do not change
unrelated fields gratuitously: completion/error/start/end do not all reset the
final-candidate flag.

Acknowledgment changes Complete to Idle only, without modifying its timestamp
or other states. It is callable only after selection/focus verification by a
future coordinator. Invalidation resets the reducer with status Unknown.

### 6.3 Hook ingress

Allow only `sessionStart`, `sessionEnd`, `userPromptSubmitted`, `agentStop`,
`notification`, `postToolUse`, `errorOccurred`.

Payload needs canonical `sessionId` and a supported `timestamp`. For notification
events recognize only `notification_type` values `permission_prompt` and
`elicitation_dialog`. Other notification types become no pending kind.
Unknown hook names are refused.

Persist only `{session_id, timestamp_ms, kind, notification}`. A non-notification
persisted hook may not carry a notification kind.

The persistent hook inbox has capacity 256. Admission requires a bound session,
a coordinator not stopped, and a supplied heartbeat with
`0 <= now_ms - heartbeat_ms <= 30000`. Reject future heartbeats. Exact duplicate
hooks are successful no-ops, even at capacity. Draining a session sorts by
timestamp stably, applies its events, and removes only those events.
Admission is not authentication or permission to replay input.

### 6.4 JSONL file tailing

Tailing is explicit over an absolute supplied file path. Poll budget is
1 through 4,194,304 bytes. Retain file identity, mtime, byte offset, bounded
partial line, and caught-up status.

Algorithm:

1. Open a regular nonlinked file using the native read boundary.
2. Reset to offset zero and a new Idle state if identity changed, file shrank,
   or the offset was at EOF and mtime changed (including same-length rewrite).
3. Read at most the budget; buffer incomplete lines.
4. Parse only newline-terminated nonblank lines. Count whitespace-only lines
   toward offsets, not record count. The 262,144-byte line bound includes its
   newline; enforce it for incomplete lines too.
5. Apply records in order; record at most 64 extracted fork observations per poll.
6. Recheck the open file and the named path. Replacement, shrinking behind the
   offset, or same-size modification during the read is a source-change error.
7. Caught up is true only at the observed end AND with no partial record.

Commit tail/reducer changes only when the entire poll succeeds. Any read,
parse, source-change, or oversized-record fault clears completeness, resets
identity/offset/partial to restart from zero next time, and invalidates the
caller's state. Never skip a corrupt line and continue from later history.
An invalid budget also invalidates the state. An empty successful poll must not
undo a previous completion acknowledgment.

Report records, bytes read, caught-up/reset flags, and minimized fork observations.
Completeness is not freshness, provenance, or action authority.

## 7. Fork-follow bookkeeping

This engine never issues a fork command. It tracks supplied evidence that a
session may have forked and decides whether to rebind an existing slot.

A parsed fork notice is a `session.info` event with `data.infoType == "fork"`.
Its message must have exactly the grammar:
`Forked this session into "<nonempty label>" (<canonical UUID>).`
Reject multiline labels and suffix text. Retain only child UUID, timestamp, and
next byte offset; discard the label.

Persist at most six pending forks. Each strict record contains:

`id`, `slot`, `source`, `binding_revision`, `process`, `issued_ms`,
`event_offset`, `seen`, `child` (required but nullable), `conflicted`.

Require nonnil intent/source UUIDs, positive process PID/creation time, at most
256 unique seen UUIDs, and a child that is nonnil, not source, and not seen.
Pending IDs and slots must be unique. Beginning a record requires the current
slot source and binding revision to match.

Observation rules:

- Ignore observations at or before saved offset.
- Advance offset before filtering old/seen/source child events.
- Ignore timestamps more than 1000 ms before issue time, saturating at zero.
- One different second child makes the pending record conflicted; never guess.

Evaluate in this order:

1. Binding revision or source mismatch -> BindingChanged.
2. Now before issue time -> ClockRegressed.
3. Age strictly over 30,000 ms without child, or 86,400,000 ms with child -> Expired.
4. Unresolved owner -> Deferred; exited or changed process lifetime -> LifetimeChanged.
5. Conflicted -> Ambiguous.
6. Incomplete discovery, over 256 candidates, or any inaccessible candidate -> Deferred.
7. Duplicate candidate UUIDs -> Ambiguous.
8. Filter to nonnil new sessions, not source/seen, same verified owner process,
   recognized interactive client, and, if a child is known, exactly that child.
9. Zero candidates -> Deferred; one -> Rebind; multiple -> Ambiguous.

Rebinding preserves label, pin, and other metadata/anchor; sets `fork_parent`
to source, changes session ID, clears initial-idle confirmation, and increments
binding revision. Do not treat the retained anchor as newly verified ownership.
Remove a pending record on Rebind, BindingChanged, Expired, or LifetimeChanged.
Keep it on Deferred, Ambiguous, or ClockRegressed.

## 8. Structural action policy, without action delivery

These are pure policy APIs over supplied evidence, not an authenticated control
path. Keep them testable without a terminal, UI automation, or a device.

UI evidence is Unknown, Composer, or Dialog.

- Composer: draft kind Empty/Conversation/Command, busy flag, 32-byte draft
  fingerprint, and 32-byte context fingerprint.
- Dialog: selection Unknown/Ordinary/SingleRequest/BroadApproval, permission
  flag, busy flag, 32-byte dialog fingerprint, and 32-byte dialog token.

### 8.1 Guard table

None is always Unsupported. For other actions, Unknown UI is refused before
status checks; Off/Unknown status is Unavailable.

| Action | Additional condition |
|---|---|
| None | Unsupported |
| Directions | Allowed structurally after the shared UI/status checks |
| Stop | Working or Waiting AND busy UI, or Waiting with a Dialog |
| Accept | Dialog; never Unknown/BroadApproval selection; permission prompt requires SingleRequest |
| Send | Idle/Complete, nonbusy Composer, Conversation draft; Empty -> EmptyDraft; Command -> CommandDraft |
| Autopilot/Fork/ModelPicker | Idle/Complete, nonbusy Composer, Empty draft |

For Accept, a permission flag OR pending PermissionPrompt makes an ordinary
selection too broad. An ordinary nonpermission dialog can be accepted even
outside Waiting, after shared checks. Do not add a stronger status restriction
that changes this pure guard's behavior.

### 8.2 Initial and final validation

Intent carries action, slot, session, process, all five generations, and
monotonic occurrence time. Evidence carries matching identity/generations,
observation time, controls-enabled/stopped/state-ready/verified/selected/
foreground/presentation-current flags, pending-input/indeterminate flags,
status, pending kind/time, run token, and UI evidence.

Initial validation order is observable:

1. Controls disabled -> Disabled.
2. Indeterminate delivery -> IndeterminateDelivery.
3. Pending input -> PendingInput.
4. Intent older than 3 seconds -> ExpiredIntent; invalid clock -> ClockMismatch.
5. Stopped or observation older than 3 seconds -> StaleState.
6. Slot/session or binding-generation mismatch -> ChangedBinding.
7. Other generation mismatch or noncurrent presentation -> ChangedGeneration.
8. Different process, zero PID/creation time, unverified target, not selected,
   or not foreground -> WrongTarget.
9. Non-direction action without ready state -> StaleState.
10. Apply guard table.
11. Stop without run token -> ChangedRun.

Exactly 3 seconds is allowed. Compare the three-second budget in widened tick
arithmetic, not rounded/truncated nanoseconds; one fractional tick beyond it
must fail.

Final validation reruns initial checks for both original and current evidence
at the current time. Require observation order to be valid. Additionally:

- Stop: same run token.
- Send: same draft fingerprint.
- Accept: same dialog fingerprint, token, pending time, and pending kind.
- Autopilot/Fork/ModelPicker: same context fingerprint.
- Directions: no extra fingerprint condition.

### 8.3 Duplicate reservations

The reservation table is process-local, capacity six session identities, no
automatic eviction. It is not persisted and is not an uncertain-input journal.
Validate final evidence before reserving. Other actions need no entry.

- Stop: refuse repeated same run forever for this reservation, and refuse a
  different run if less than 3 seconds since previous Stop reservation.
- Accept: refuse same pending time AND dialog token for less than 3 seconds.
  A changed request/token is a different reservation.
- Reject invalid clock comparison rather than clearing prior suppression.
- Exactly 3 seconds allows the time-limited cases.

These methods must not spawn input workers or inject keys even when they return
success.

## 9. Durable state and recovery

### 9.1 API and schema

State storage is library-only. There is no public CLI for production migration,
initialization, mutation, or recovery. A caller supplies an existing approved
absolute local root; no implicit home-directory or temp-directory default.

The Rust API is `persistence::state_store::StateStore`: callers explicitly
initialize, load, or transact with an expected store revision. Recovery is
never automatic; callers inspect `plan_recovery(expected_revision)` and pass
the returned artifact token with an explicit `KeepCurrent` or
`CompletePrepared` decision to `recover`. The token binds the decision to the
pending/replacement file identities and bytes observed while holding the lock.
The separate `persistence::ownership::RuntimeOwnerLease` takes a
caller-selected authority root, independent of each configuration root, and
locks its existing empty `runtime-owner.lock` anchor without writing owner data
or taking over a held lock.

Files:

- `core-state.json`: committed snapshot.
- `core-state.pending.json`: prepared snapshot/recovery evidence.
- `core-state.pending.json.replacement`: Unix interrupted replacement candidate.
- `core-state.lock`: empty OS-lock file.

Strict snapshot schema:

```json
{
  "version": 1,
  "store_id": "11111111-1111-4111-8111-111111111111",
  "revision": 0,
  "data": {
    "settings": {"version": 1},
    "bindings": {"version": 1, "revision": 0, "slots": {}},
    "pending_forks": [],
    "hooks": []
  }
}
```

The example omits defaulted preference fields; normal serialization may emit
them. Maximum snapshot size is 262,144 bytes. Refuse unknown nested schema,
invalid bounded records, unsupported versions, nil store ID, invalid forks, or
overfull hook queue. Snapshot revision and binding revision are separate.

### 9.2 Transaction semantics

Acquire an OS file lock, not a lockfile-existence convention. Require the lock
file be empty and its named identity still match the held handle. Try 20 times,
sleeping 10 ms between contention attempts; then Busy. Validate root/lock
before and after publication.

Load blocks with RecoveryRequired if pending or replacement evidence exists,
even if committed state is valid. Missing state is NotInitialized, not defaults.
Corrupt/unknown state is preserved.

Initialization is explicit: refuse an existing snapshot, pending evidence, or
any root entry besides the lock. Create a fresh nonnil UUID and revision zero.

Transaction:

1. Lock and load current disk state.
2. Compare expected store revision before running the mutation closure.
3. Check revision overflow.
4. Run the closure exactly once on private in-memory data under the lock.
5. Validate and serialize the entire combined snapshot.
6. Exclusively create the pending file; write and synchronize it.
7. Atomically replace the current snapshot, or atomically publish initial state.
8. Confirm the lock/root still match.

Closure failure publishes nothing. A write/sync/replace/final-identity failure
can be CommitIndeterminate. Preserve prepared or partial evidence; do not retry
the logical mutation, delete evidence, truncate old state, or guess defaults.
Old readers must see their old complete snapshot, not partially overwritten JSON.

On Windows, retry only sharing/lock violation replacement errors (codes 32/33),
at most eight attempts with delays `10,20,40,80,160,160,160` ms. Other errors
are not retryable. Unix replacement errors are not automatically retried.

### 9.3 Explicit recovery

Require the caller's expected current revision, including expected absence.
Require a valid pending snapshot.

- If committed and prepared snapshots are equal, synchronize current and discard
  matching prepared evidence. Return the existing revision, never increment it.
- Otherwise require same store ID and exactly next revision; with no committed
  snapshot, require pending revision zero.
- KeepCurrent preserves current and explicitly discards valid matching
  prepared evidence. It cannot discard an uncommitted initialization.
- CompletePrepared publishes prepared and returns its revision.
- Unrelated store, skipped revision, corrupt/unknown pending state, orphaned
  replacement, or mismatching replacement bytes blocks recovery and writes.
  Preserve every unrecognized artifact byte-for-byte.

### 9.4 Native root contract

All platforms reject unsafe basenames: empty, `.`, `..`, separators, NUL, colon.
State files must be ordinary single-link files, never directories, symlinks,
reparse points, pipes, or hardlinked files.

Windows:

- Implementation limitation: the current private-root boundary supports
  read-only access and locking an existing anchor, **not durable mutations**,
  even on fixed local disks. `check_mutation_support`, exclusive creation,
  creation of a missing lock anchor, publication, and recovery discard fail
  closed with `UnsupportedPlatform` /
  `windows_directory_durability_unavailable` before changing the namespace.
  The unsupported guarantee is crash/power-loss persistence of directory-entry
  creation, replacement, and removal, including evidence cleanup. Flushing a
  file handle alone does not establish that guarantee; successful directory
  `FlushFileBuffers` is not a documented substitute, `ReplaceFileW` has no
  supported write-through flag, and initial write-through move alone does not
  cover the complete protocol. Volume-wide flushing requires privileges and
  is not implemented. Do not claim Windows persistence acceptance (including
  S03 publication/old-reader acceptance) until a supported complete strategy
  is implemented and tested natively.
- Only absolute drive-letter paths on fixed local disks.
- Pin every ancestor using directory handles without delete sharing; refuse
  reparse ancestors. Do not claim network/removable semantics.
- Open state with reparse-safe flags and compare volume/file identity.
- Existing replacement uses the native replace-file operation; initial
  publication uses write-through move. No delete-then-rename fallback.
- Read handles can permit delete sharing; lock/ancestor handles prevent
  replacement of their own names while held.

Unix:

- Walk absolute path components with descriptor-relative no-follow opens; reject
  parent traversal and symlink spellings, including symlinked ancestors.
- All writable ancestors must be safe: no group/other writable ancestor except
  root-owned sticky directories.
- State/authority directories belong to the effective user and have no
  group/other permissions, normally mode 0700. Files similarly normally 0600.
  Reject unsafe permissions; never repair them silently.
- Reject macOS extended ACLs conservatively.
- macOS: local APFS only. Linux: ext-family, XFS, or Btrfs only. Refuse network,
  FUSE, overlay, and unknown filesystem semantics.
- Retain root descriptor and original device/inode identity. Revalidate path,
  root, and named files. A descriptor does not prevent directory renaming;
  root replacement fails closed without writing to a substituted directory.
- Create files exclusively relative to that descriptor, with nonblocking,
  close-on-exec, and no-follow flags.
- Synchronize files and directory entries. macOS durable file synchronization
  includes full filesystem flush.
- Keep prepared evidence while copying to an exclusive `.replacement` file.
  A preexisting replacement must be byte-identical to prepared state.
  Synchronize replacement, atomically rename, synchronize directory, then
  remove prepared evidence. Post-rename sync failure is indeterminate.
- Explicit discard may remove a replacement only after bounded byte-for-byte
  comparison with prepared evidence.

Read-only event tails need not have private file permissions or use one of the
state filesystems, but still reject unsafe links/types and unsafe path walking.
The native tail boundary exposes handle-derived last-write and change timestamps
as an opaque equality token alongside file identity and length. Pollers retain
this token to reset at a changed EOF. Reads reject same-size timestamp changes
during a read or since the last successful read/open, while ordinary appends
remain permitted. Reopen after a source-change error and reset reducer state.
These timestamps are not a content digest or an unforgeable generation: native
timestamp precision and update behavior still apply.
This boundary is not a sandbox against malicious code running as the same user.
Crash-injection tests are not physical power-loss qualification.

### 9.5 Compatibility import/export APIs

Operate only on supplied settings/binding bytes, never discover production files.
Permit a UTF-8 BOM only at this compatibility boundary. Preserve preferences,
labels, pin, session identity, and other compatible metadata, but clear PID,
process-created value, target anchor, and initial-idle confirmation. Reset
binding revision to zero. Do not import/export pending forks, hooks, event
reducer state, ownership, or action authority. Output settings and bindings as
separate byte strings. This is not a production migration feature.

## 10. Six-slot presentation

### 10.1 Semantic snapshot

A snapshot contains epoch, generation, connected flag, and exactly six slots.
Each slot contains:

- Occupancy: Empty or Bound.
- Status: the seven session states.
- Availability: Live, Stale, Exited, PidReused, Unresolved, Busy.
- Selection: None, Pending, Confirmed.
- Cursor boolean.
- Authority: Disarmed, ObserveOnly, ActionsGranted.
- Optional overlay: Refused(reason), Accepted, or Attention, with generation
  and source-tagged monotonic start time.

A latest-snapshot holder accepts only its nonnil expected epoch and strictly
increasing generations. Old epochs and duplicate/lower generations cannot
resurrect an old view.

### 10.2 Token priority and selection

Determine overlay validity: same generation and valid age strictly below two
seconds. Expire exactly at two seconds. A bad clock never preserves an overlay.

Choose the visible token in this order:

1. Disconnected or invalid/mismatched epoch clock -> Neutral.
2. Empty occupancy -> Empty.
3. Stale/Exited/PidReused/Unresolved availability -> corresponding token.
4. Valid Refused overlay -> Refused.
5. Busy availability -> Busy.
6. Otherwise session status token.

Count provider selection markers before filtering stale slots. More than one
Confirmed clears all Confirmed markers; more than one Pending clears all Pending
markers. Resolve each kind independently: one Confirmed and one Pending on
different slots are valid. Even a stale duplicate counts toward ambiguity.
Affected slots lose selection, cursor, and visible authority.

Neutral, Empty, Stale, Exited, PidReused, and Unresolved also clear selection,
cursor, authority, and overlay. Do not mutate the underlying provider snapshot.

Effects: stale/unavailable -> Steady; Refused/Waiting/Error or Attention overlay
-> Attention; otherwise Confirmed selection or Working -> SlowPulse; else Steady.
The effect is a request, not proof of a supported native animation.

### 10.3 Canonical colors and styles

All colors are literal sRGB bytes; do not silently substitute device-native
palettes or older status colors.

| Token | Logical RGB | Screen fill | Screen text | Word |
|---|---|---|---|---|
| Off | 0,0,0 | 97,97,97 | white | OFF |
| Idle | 255,255,255 | same | black | IDLE |
| Working | 32,112,255 | same | black | WORKING |
| Complete | 0,208,96 | same | black | DONE |
| Waiting | 255,170,0 | same | black | WAITING |
| Error | 255,48,48 | same | black | ERROR |
| Unknown | 96,125,139 | same | black | UNKNOWN |
| Empty | 0,0,0 | 48,48,48 | 176,176,176 | EMPTY |
| Stale | 93,64,55 | same | 255,213,79 | STALE |
| Exited | 93,64,55 | same | 255,213,79 | EXITED |
| PidReused | 93,64,55 | same | 255,213,79 | PID REUSED |
| Unresolved | 93,64,55 | same | 255,213,79 | NOT FOUND |
| Busy | 106,27,154 | same | white | BUSY |
| Refused | 191,54,12 | same | white | REFUSED |
| Neutral | 96,125,139 | same | black | OFFLINE |

Confirmed selection uses white and word SELECTED, solid treatment. Pending uses
RGB 0,229,255 and word PENDING, dashed treatment. Status and selection are
separate visual channels; a marker must not erase the status word.

Create original status glyphs for Idle, Working, Complete, Waiting, Error,
Unknown. Give each a stable semantic ID (`state-idle`, etc.) and verify bundled
SVG bytes against a newly generated SHA-256 manifest. The test is consistency
with the new manifest, not equality with externally sourced images. No external
asset loading at runtime. Other tokens need no status glyph.

### 10.4 Capability evidence

Scope fields: device, profile ID, firmware, transport (USB/BLE/Serial), mode or
layer, platform, and architecture. Full scope requires nonblank profile,
firmware, mode, platform, and architecture, each at most 128 bytes and without
control characters.

Evidence labels: NotImplemented, Inferred, SourceReported, VendorClaim,
OfflineDecoded, MockTested, NativeObserved, PhysicallyAccepted.

Exact scope equality is mandatory. Do not transfer observations between device
models, firmware, layers, transport, OS, or architecture.

| Use | Minimum accepted categories |
|---|---|
| Presentation planning | MockTested, NativeObserved, PhysicallyAccepted |
| Input admission | NativeObserved or PhysicallyAccepted AND complete scope |
| Visible-output qualification | PhysicallyAccepted AND complete scope |

NativeObserved and PhysicallyAccepted additionally need a nonempty bounded
observation reference. A successful ACK is not physical visibility. Historical
captures are not a current scope grant.

Default descriptors deliberately have no firmware or mode and therefore cannot
admit live input. Base presentation features are mock-tested when implemented;
input capabilities are only offline-decoded. SlowPulse is initially
NotImplemented. Words and selection frames are absent on Micro; icons are
implemented in AKP03 semantic SVG only; brightness is represented only on Micro.

### 10.5 Effect negotiation and fallbacks

Preferences: brightness 0.25, reduced motion false, high contrast false, show
words true. Reject nonfinite or out-of-range brightness.

Steady request resolves to Steady. Other requests fall back to StaticEmphasis
with ReducedMotion or EffectUnqualified unless all native-pulse conditions hold:
Micro device, SlowPulse request, same exact token/scope, SlowPulse capability
physically accepted, observation reference, measured smooth cycle >=1000 ms,
finite positive native speed, and reduced motion false.

Do not synthesize a strobe or assume a firmware effect number is a slow pulse.
Reduced motion wins even over qualified native effects. Attention has no
qualified native realization and takes the safe fallback.

Realized slots expose semantic data, RGB, style, effect realization and reasons,
effective brightness, optional tile size/LED entry, border width, and fallbacks.

Device-specific fallback rules:

- Micro is color-only. Confirmed selection uses configured brightness; others
  use half. Nonconfirmed StaticEmphasis becomes Steady and reports that static
  emphasis was not applied. Empty and Off are visually indistinguishable and
  explicitly flagged. Pending/cursor information that cannot be shown is
  flagged. Emit no LED wire frame without qualified steady output; native
  animation needs its separate qualification.
- AKP03 keeps words even when show-words is false, reporting WordsRequired.
  Brightness is not applied. Original profile is 60 px; rev2 is 64 px. Logical
  SVG is upright; only the hardware JPEG path rotates.
- CT keeps words, reports unrendered artwork and unapplied brightness. Neutral
  renders using the Stale tile style and reports this loss. High contrast and
  static emphasis are not actually applied and must be flagged.
- Borders are nominally 2 px, or 4 px under high contrast; report when a device
  cannot actually apply a preference.
- Accepted overlay is not rendered and must be reported. Cursor-not-shown
  fallbacks must not duplicate themselves.
- Every unqualified output reports OutputUnqualified rather than implying a
  synthetic render has been physically displayed.

The synthetic shared demo uses six Bound/Live/ObserveOnly slots in order:
Idle, Working, Complete, Waiting, Error, Unknown. Slot 1 is Confirmed; slot 3 is
Pending. Use a valid fixed synthetic clock/epoch; do not enumerate real sessions.

## 11. Logical input and candidate assignment

### 11.1 Controls, roles, and validation

Controls have stable ID, kind (Press, Turn, Stick, TouchSlide, Wheel), optional
recommended intent, near-selection flag, isolated-stop flag, and adjacency IDs.
Device-specific IDs and normalizers are in the appendices.

Intents:

- SelectSlot(1..6)
- MoveCursor
- CommitCursor
- Command(Action)
- ShowHealth
- PageNext / PagePrevious

Externally tagged snake-case JSON represents valued intents, for example
`{"select_slot":1}` or `{"command":"stop"}`; unit variants are strings such as
`"move_cursor"`.

Priority order: P0 direct slot selection; P1 cursor move/commit; P2 Stop;
P3 Accept/Send; P4 directional navigation; P5 everything else. Sort unassigned
controls last. Priority is not evidence or authority.

Shape validation:

- At most 64 bindings, no duplicate control assignment, known controls only.
- None action is unsupported.
- MoveCursor needs Turn or Wheel.
- SelectSlot and CommitCursor need Press.
- Stop/Accept/Send/Fork are sensitive and need Press, not continuous controls.
- Stop needs a physically isolated Stop control. Reject Stop next to Accept
  or Send if either direction of the adjacency graph says adjacent.
- Sensitive controls near selection need explicit adjacency confirmation.
- Full activation additionally needs complete matching scope and native/physical
  input evidence for every bound control.

### 11.2 Clean-release tracker

Physical events contain control ID, Press/Release/Turn(i8), connection generation,
and source-tagged monotonic sample. Admit only unit turn deltas -1 or +1.
Uncalibrated stick motion and touch sliding do not become discrete input.

A clean press gesture requires one matched press/release of the same control and
generation within 3 seconds, same scope, unchanged entire binding list, valid
clock source/epoch/frequency/order, and no overlapping held control.

Tracker behavior:

- Connection-generation mismatch -> ChangedGeneration.
- Clock invalidity/change/regression -> ClockMismatch; discard the candidate
  press and taint the gesture until reset/clean release.
- Unknown control -> Unsupported and taint.
- Duplicate press is ignored, not a second gesture.
- Maximum 64 held controls; overflow -> Busy.
- Overlapping presses taint the whole overlap; no release commits it.
- Unmatched release is ignored.
- Clear held candidate/scope/bindings/taint when all held keys have released.
- Turn while anything is held is ignored and taints the overlap.
- Nonunit turn is ignored.
- A clean release/turn must still match its control kind and a valid fully
  evidenced binding.
- Debounce per control at 300 ms for noncommands, 500 ms for commands. Values
  exactly at the boundary are accepted; rejected duplicates do not reset time.
- After successful shape/evidence/debounce validation, command intents still
  return Disabled. Noncommands return a logical Request with delta 0 or +/-1.
- Explicit reset on disconnect, mode change, or clock cut replaces all gesture
  state. No stale held input may survive into a new connection.

Micro's separate microphone coalescing remains 400 ms; do not replace it with
the generic debounce.

### 11.3 Offline assignment and export

Read stdin to at most 32,769 bytes, reject if over 32,768. Strict input:

```json
{
  "device": "ajazz",
  "intent": {"select_slot": 1},
  "adjacency_confirmed": false,
  "events": [
    {
      "control": "DEVICE_SPECIFIC_CONTROL_ID",
      "kind": "press",
      "connection_generation": 1,
      "at": {
        "source": "linux_monotonic",
        "stamp": {
          "epoch": "11111111-1111-4111-8111-111111111111",
          "ticks": 0,
          "frequency": 1000
        }
      }
    }
  ]
}
```

This shows the schema, not a valid complete press gesture: supply the matching
release or exactly one unit-turn event for a turn control. No events,
duplicates, overlaps, more than one gesture, holds over 3 seconds, mismatched
generation/clock/control kind, unsafe role, or unknown field are refused.

Output assignment contains version 1, device, profile, logical binding,
candidate-only activation statement, and blocking reason if activation is not
qualified. Validating shape does not upgrade evidence.

Export requires both `--export-root` and exact `--confirm ASSIGN`.
Use the native private-root boundary and exclusive creation of
`device-binding.json`; synchronize and verify identity. Never overwrite an
existing artifact. Preserve a partial write on failure. Export no recorded
events, timestamps, host/session/process identity, observation grants, or action
authority. Do not automatically load the file or change production settings.

## 12. Straightforward implementation plan

Prefer one binary and one library, direct enums/structs, synchronous bounded
device protocols, and small injectable I/O boundaries. Do not add a plugin
registry, network service, background runtime, GUI framework, or dependency
injection framework.

Suggested organization is intentionally not an internal API compatibility
requirement:

```text
Cargo.toml / Cargo.lock / rust-toolchain.toml
src/
  lib.rs
  main.rs
  domain/          # identities, settings, state, policy, forks
  persistence/     # bundle transactions, recovery, native root operations
  presentation/    # semantic view, original artwork, fallback/evidence logic
  devices/         # micro, akp03, ct; wire codecs separate from live I/O
  native/          # clocks, observations, owner locks, worker supervision
assets/            # newly authored glyphs and their hash manifest
tests/
```

Suitable initial dependency selections:

| Purpose | Selection |
|---|---|
| CLI | `clap` 4.5 with derive |
| Structured data | `serde` 1 with derive; `serde_json` 1 |
| IDs | `uuid` 1 with serde and v4 |
| Digests | `sha2` 0.10 |
| JPEG | `jpeg-encoder` 0.6 |
| Unicode categories | `unicode-general-category` 1.1, Unicode 16.0 |
| HID | Optional `hidapi` 2.6 on Windows/macOS |
| Native Windows APIs | `windows` 0.62, only required API feature groups |
| Native Unix APIs | `libc` 0.2 |
| USB identity property lists | Optional `plist` 1.8 on macOS |

These are compatible starting ranges, not a requirement to duplicate a
particular dependency graph. Lock resolved versions. Use OS process and
filesystem primitives where needed. No scripting-language runtime is needed by
the executable.

### Phase 1: Contracts and pure engine

Implement slots/time/enums, strict settings/bindings, reducer, minimized
event/hook parsers, fork evaluation, policy, reservations. Add boundary-focused
tests while implementing. No native device access.

Exit: strict parsing, timestamp vectors, transition tables, revision/ownership
decisions, and action-policy tests pass. Neither tests nor library send input.

### Phase 2: Safe persistence and native services

Implement private root primitive first, then atomic bundle store and explicit
recovery. Reuse root handling for candidate exports and runtime-owner anchors.
Implement clocks, process/window observation, lifecycle invalidation, target
matching/qualification, and bounded workers from the platform appendix.

Exit: competing-process tests, process-death release, root substitution,
permissions/link refusal, and crash-point recovery tests pass on each supported
native OS. Unsupported cases return typed failures.

### Phase 3: Shared presentation and offline devices

Implement palette/view/evidence/effect/gesture/assignment, then all three
device codecs, tile models, image transforms, and synthetic previews. Author
independent glyphs with a new hash manifest.

Exit: golden protocol vectors, normalizer tests, JSON/SVG geometry checks,
effect/fallback tests, and CLI offline/no-action tests pass without hardware.

### Phase 4: Opt-in diagnostics

Implement HID enumeration/probes and native CT serial experiments behind
explicit commands/features. Keep framing/parsing independently mockable.
Use bounded supervised children, write-opt-in checks, cleanup, and failure
progress reports.

Exit: fake-transport fault matrix and child containment tests pass. Health
commands are usable without a connected device. Native observation does not
upgrade qualification.

### Phase 5: Optional firmware maintenance

Implement local pinned-artifact plan, strict update options, USB identity,
private verified backup, partition/settings preservation, supervised flashing,
and revalidation exactly as specified in the maintenance appendix.

Exit: offline/fake-tool tests prove no flash before every prerequisite.
Real-device acceptance is a separate explicit maintenance exercise; it is never
part of default CI.

### Phase 6: Package and parity closeout

Run the matrix below, document native prerequisites, confirm no forbidden
action surface, and record any physically unqualified combination honestly.
Do not add an installer or production cutover as a hidden last step.

## 13. Test and acceptance plan

Build tests from this specification, with synthetic UUIDs, clocks, process
identities, event files, and fake transports. Tests must not discover personal
sessions, send terminal commands, write hardware, or update firmware.

Each acceptance item below is mandatory, in addition to the appendix-specific
vectors. Use table-driven tests rather than one test per trivial accessor.

Synchronize competing-process tests with explicit barriers or pipes, not
assumptions about filesystem speed. Test Busy while another process deliberately
holds the lock, and StaleRevision after lock acquisition, as separate outcomes.
A bounded lock wait can legitimately expire during slow synchronization; do
not change the production locking contract to make a scheduling-dependent test
pass.

| ID | Test family | Required cases |
|---|---|---|
| C01 | Strict settings | Default vs recommended; partial binding overrides; unknown/duplicate keys; null/type/version/bounds |
| C02 | Labels and anchors | Multilingual/combining marks; all rejected Unicode categories; scalar count; anchor integers; path edge cases |
| C03 | Slots | Stable assignment; pinned/live/unresolved never evicted; duplicate session; same-session refresh; stale/overflow revision |
| E01 | Timestamps/presence | ISO offset known answer; leap days; floats/truncation; empty/nonempty containers; privacy minimization |
| E02 | Reducer | Final-message vs tool turn; abort/new run; equal/old timestamps; completion survives late tool hook; acknowledgment |
| E03 | Tail | Split lines; bounded catch-up; blank lines; EOF rewrite; truncation/replacement; malformed/oversize; no skipped history |
| E04 | Hook inbox | Bound/unbound; stopped/future/stale heartbeat; exact duplicate at capacity; stable drain; no content retention |
| F01 | Fork follow | Offset/age filtering; owner unresolved/exited/reused; one/multiple candidates; child lock delay; both deadlines; pin/label preservation |
| P01 | Guards | Each action's valid case; unknown UI; command/empty draft; broad permission; busy/stop; no delivery |
| P02 | Revalidation | Every generation/identity/fingerprint/run/request change; 3-second exact/fractional boundary; all clock errors |
| P03 | Reservations | Same-run Stop; new-run cooldown; Accept request/token cooldown; six-session bound; clock cuts; no journal claim |
| S01 | Store | Initialize/load; compare disk revision under lock; failing closure; max size; unknown/corrupt state preserved |
| S02 | Recovery | Prewrite/partial write/pre-rename/post-rename failure; equal snapshots; Keep/Complete; orphan/mismatching replacement; unrelated ID |
| S03 | Native files | Links, FIFOs, unsafe roots/ancestors, permissions, ACLs, hardlinks, renamed root/lock, old open reader |
| S04 | Multi-process | Two writers; owner contention across config roots; process death; no lease takeover |
| V01 | Semantic view | Exact palette/words; priority order; 2-second overlay expiry; wrong epoch/generation; duplicate Pending/Confirmed independence |
| V02 | Evidence/effects | Every scope dimension; incomplete scope; observation refs; native vs physical; exact token; >=1-second pulse; reduced motion |
| V03 | Realization | Three profiles; original/rev2 sizes; words retained; all declared fallback reasons; no unqualified LED frame |
| I01 | Gesture tracker | Duplicate press, overlap, long hold, changed scope/bindings, clock/generation cuts, discrete kind checks, unit turns |
| I02 | Role policy | Limits; sensitive continuous input; isolated Stop; adjacency both directions; priority sorting; commands remain Disabled |
| I03 | Assignment | Exact one gesture; strict 32-KiB input; candidate-only output; confirm pairing; exclusive/private export; no authority leakage |
| D01 | Device codecs | Every appendix golden vector plus split/short/malformed/overflow/unsupported report |
| D02 | Probe faults | Timeouts, cancellations, short writes, malformed replies, no optional writes without opt-in, cleanup and progress snapshots |
| M01 | Maintenance | Artifact pins; private backup; USB identity; partition/settings preservation; every precondition before flashing; failure report |
| CLI01 | Commands | Help/version; offline output shapes; unavailable != empty scan; failed report exits nonzero; workers hidden |
| CLI02 | No action path | Reject all action-delivery command names; inventory and previews always not ready; no implicit runtime |

### Native matrix and commands

Run default and all-feature tests on native Windows x64, macOS ARM64, and Linux
x64. Cross-check Windows ARM64 compilation separately; that is not runtime or
hardware acceptance.

Set `CARGO_TARGET_DIR` to an explicit approved absolute build directory.
Resolve symlink ancestors before choosing Unix fixture paths. Every fixture
creates only its own uniquely named child under that root; create private roots
with the required modes. No fallback to production state or an implicit system
temporary directory.

```sh
cargo test --locked --all-targets
cargo test --locked --all-targets --features akp03-hid,micro-hid,micro-firmware
cargo fmt --all -- --check
cargo clippy --locked --all-targets --features akp03-hid,micro-hid,micro-firmware -- -D warnings
cargo build --locked --release --features akp03-hid,micro-hid,micro-firmware
cargo check --locked --all-targets --target aarch64-pc-windows-msvc
```

Hardware experiments are separate, bounded, manually opted-in acceptance:

- CT serial framing/query/reopen, then individually observed controls and
  display geometry. Correlated replies alone are not visible-output proof.
- AKP03 rev2 descriptor/capture, initialization, six numbered tiles, encoder
  directions, and explicit releases. Do not assume simultaneous-key rollover.
- Micro qualified collection, query replies, LED visibility, active-layer input,
  USB/BLE reconnect behavior. Do not generalize one transport to another.
- Firmware only after explicit maintenance authorization, verified full backup,
  and a recovery plan using the specified local artifacts.

Record device/profile/firmware/transport/mode/OS/architecture with every physical
observation. No historical qualification silently becomes a current grant.

### Completion checklist for the implementing session

The repository is complete when the documented command set, pure APIs,
persistent-state semantics, device wire codecs/renderers, platform services,
optional bounded probes, and optional guarded updater are implemented and
covered by the above matrix. All unimplemented host actions must remain
explicitly unavailable, all writes require the specified opt-ins, and all
unsupported/physically unqualified combinations must be reported accurately.

Do not call a narrower Micro-only implementation complete: both display-device
families, portable persistence, presentation/evidence/input rules, and platform
diagnostic containment are part of the required Rust functionality.
