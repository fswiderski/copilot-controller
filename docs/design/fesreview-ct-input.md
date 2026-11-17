# fesReview CT input: offline candidate block

**Status: review-ready lower layer only; no CLI route ships.** No hardware
was accessed or accepted for this block. Creator Micro 2 and the app's action
dispatcher are separate work. This does not change production device admission,
the broker, or the diagnostic worker protocol.

## Present boundary

This PR adds no command, help entry, worker route, feature flag, or enable switch.
`review-input --device ct` remains unsupported by the existing CLI.
The eventual adapter must be explicitly invoked on demand by the app, never
started implicitly at app startup. It is not implemented by this candidate PR.
Do not use `ct-probe` as an app input fallback.
`ct-probe` returns a **final timed diagnostic dump**, including samples collected
while querying identity/firmware. That dump is not a live input stream, and its
sample timestamps are not action-authority timestamps.

## Implemented offline lower layer

`devices::ct::review_input::CandidateStream` reuses the clean-room CT
`FrameDecoder` and strict `decode_input`. It accepts raw framed input bytes only,
after a future transport has established a fresh, admitted input boundary.
It has no native discovery, handle, ownership capability, I/O, callback, timer,
thread, queue, action dispatch, or transport implementation.

Proposed mappings (all **candidate pending physical acceptance**):

| CT control | Raw ID | Proposed action |
|---|---|---|
| Round 1 | `07` | `toggle_play` |
| Round 2 | `08` | `seek_start` |
| Round 3 | `09` | `seek_end` |
| Round 4 | `0a` | `back_one_second` |
| Round 5 | `0b` | `forward_one_second` |
| Round 6 | `0c` | `screenshot` |
| Round 7 | `0d` | `toggle_dictation` |
| Center wheel | `00` | negative delta: `previous_frame`; positive: `next_frame` |

CT round 8, raw ID `0e`, is intentionally unused and out of scope.

Each signed delta unit produces one candidate action (including magnitude 128).
Physical direction and units-per-detent are not established. Round 1 has one
historical captured pair; rounds 2–7 and the wheel have source-reported mappings,
not measured acceptance for this workflow. Other buttons, knobs, touches, and
ticks produce no actions. Identity/firmware replies are rejected as non-input.

Buttons start disarmed: an Up must precede a fresh Down/Up pair, which emits only
on release. Thus a first press can intentionally be lost until a release is
observed. Repeated edges do not emit twice. A new Down within 75ms of emission
is suppressed through its release, even if held beyond the cooldown. This is a
conservative **synthetic candidate policy**, not a physically qualified debounce
interval. Transaction IDs are not assumed to be unique event sequence numbers.

Each call accepts at most 4096 bytes, 128 frames, and 128 expanded actions.
Framing retains at most one 255-byte payload. There is no persistent action
queue. Oversize input, action/frame overflow, malformed input/framing, and
monotonic-clock regression discard the entire current batch and permanently
end the stream. EOF rejects a partial frame and always discards held presses.
Explicit `end()` handles cancellation, transport failure, or foreground loss.
There is no reset/reconnect API: a new stream is a new disarmed instance.
Returned candidate batches are not delivery grants; the caller must discard
outstanding batches at any authority or stream boundary.

`CandidateAction` defines the proposed strict action-only vocabulary and bounded
NDJSON encoding, for example `{"action":"toggle_play"}\n`. Unknown/duplicate fields
and unknown actions are rejected. No readiness record has been introduced.
No executable emits these candidate records in this PR.

Validation: the candidate module contains nine `#[test]` functions, all passed.
Separate existing CLI regressions were also run; they are not additional
candidate tests.

## Evidence already present versus evidence still needed

The [recorded physical evidence](../../specs/DISPLAY_DEVICES.md#recorded-physical-evidence)
already establishes a native macOS ARM64 query/close on one USB `2ec2:0003`
unit reporting firmware **`[0,2,8]`**, a 28-byte identity response, and one
guided round-1 `07` Down=`00` / Up=`01` pair. That firmware version is not merely
synthetic: it also appears in synthetic tests, whose `TEST-DEVICE` identity
must not be confused with the physical unit. No new hardware run was performed
for this PR.

| Gate | Exact missing evidence or implementation |
|---|---|
| Current device/firmware | A fresh successful baseline on the operator's unit, correlated with the new captures. Existing native discovery checks exact VID/PID, CDC interface descriptors, unique endpoint and post-open registry association. The engine does **not** pin firmware to `[0,2,8]`. A lab admission policy must pin the verified tuple before forwarding input; do not change production admission. |
| Queried identity | Current output retains length only, not the private identity body; equal lengths do not prove equal units/models. If exact queried bytes are required by the chosen admission policy, the existing CLI cannot supply that evidence. A privacy-reviewed in-worker comparison/attestation is required, not a public raw-identity dump or user-selected port. Do not invent a serial value. |
| Seven buttons | Round 1 has one captured pair; round **2–7 have no measured pairs** here. Obtain isolated, labeled repeated Down/Up captures for IDs `08..0d`, plus round 1 as a same-session reference. Verify no missing releases, hold repeats, extra edges, or unintended control IDs. |
| Center wheel | No measured center-wheel rotation here: verify ID `00`, clockwise/counterclockwise sign, signed delta per physical detent, and whether multiple detents coalesce. Do not assume one report or one delta unit equals one detent. |
| Candidate state policy | The 75ms cooldown and initial Up-to-arm behavior are synthetic. Record holds, quick repeated presses, and entry while held for **each** mapped button; compare raw edges with intended pair counts before accepting/tuning this policy. The minimal mapping capture below alone does not qualify debounce. |
| Stream failures | Fragmentation, malformed frames, limits, EOF and clock regression are synthetic-tested here. Still needed in the lab route: pipe backpressure/EOF, supervisor loss, transport cancellation/disconnect, and ownership exclusion tests without action dispatch. |

Display/LED acceptance, electrical line measurements, and general CT touch
qualification are not prerequisites for this input-only lab. They must not be
claimed or enabled. `devices::ct::require_live_support` still returns
`HardwareUnqualified` for the expected VID/PID; this PR does not bypass it.

## Operator capture using the existing executable (not run for this PR)

CT hardware capture requires separate explicit authorization and has not been
authorized for this work; permission for Micro capture does not authorize CT.
The procedure below is preparation only, not permission to execute it.

Use the native macOS ARM64 executable, no HID features required. Close any
vendor/controller application using the CT and connect only the intended CT.
Do not start concurrent probes or use a vendor executable, guessed port,
firmware tool, `sudo`, or `--test-pattern`.

Build only (this command does not touch hardware):

```sh
cd /Volumes/Storage/src/copilot-controller-fesreview-ct
CARGO_TARGET_DIR=/Volumes/Storage/build/copilot-controller-fesreview-ct \
  cargo build --locked --bin copilot-controller
```

The following are **operator-only hardware commands**, not automatic tests.
Store captures in a new local directory under the worktree; do not publish
device-identifying material. Start with one baseline, hands off:

```sh
mkdir -m 700 ct-capture
/Volumes/Storage/build/copilot-controller-fesreview-ct/debug/copilot-controller \
  ct-probe --schema-version 1 --total-timeout-ms 8000 --io-timeout-ms 1000 \
  --capture-ms 0 --max-events 128 --max-received-bytes 32768 \
  > ct-capture/baseline.json 2> ct-capture/baseline.stderr
```

Continue only on exit zero, empty stderr, native evidence, HTTP 101,
`pattern: "not_requested"`, `close: "sent"`, and the expected firmware `[0,2,8]`.
Retain the baseline identity length, but do not use it as an identity check.
If the firmware differs, stop acceptance and record it as an unverified tuple.

For each labeled capture, invoke this **same** bounded command, replacing
`round-1` in both filenames with the label in the sequence below:

```sh
/Volumes/Storage/build/copilot-controller-fesreview-ct/debug/copilot-controller \
  ct-probe --schema-version 1 --total-timeout-ms 8000 --io-timeout-ms 1000 \
  --capture-ms 5000 --max-events 128 --max-received-bytes 32768 \
  > ct-capture/round-1.json 2> ct-capture/round-1.stderr
```

Wait for each process to exit and inspect its report before the next invocation.
There is **no live capture-ready cue** in the existing command. Keep hands off
during startup; aim to perform the labeled actions about 2–4 seconds after
invocation, finishing before 4.5 seconds. This timing is an operator aid, not
proof that capture has begun; the phase timestamps below decide acceptance.

| Sequence / filename label | Physical action during that one capture |
|---|---|
| `round-1` through `round-7`, seven separate runs | On that button only, three deliberate press/release pairs, roughly one pair per second; leave it released. IDs expected in order: `07,08,09,0a,0b,0c,0d`. |
| `wheel-cw` | Rotate only the large bottom-center wheel clockwise through four individually counted detents, roughly one detent per half-second; do not push it. |
| `wheel-ccw` | Repeat four individually counted detents counterclockwise, without pushing. |

Record which control was touched, counted detents, direction, and approximate
physical timing in the capture review. Touch reports from wheel contact are
allowed as annotated non-action input; never reinterpret them as rotation.
If the wheel has no countable detents, stop calling delta units “detents” and
document the physical unit actually measured.

Each successful JSON has the final report at `.ct_probe`. Inspect:

* `evidence == "native_transport_observation"`, `firmware_version == [0,2,8]`,
  `http_101 == true`, `pattern == "not_requested"`, `close == "sent"`,
  `completion == "capture_duration_elapsed"` and empty stderr.
* `.progress.phases[]` with `phase == "capture"` gives `started_ms`. Only accept
  samples with `.elapsed_ms >= started_ms` and
  `.elapsed_ms < started_ms + 5000`; queries can collect earlier samples.
  Reject ambiguous runs or limit-terminated captures, not just malformed JSON.
* `.inputs[]` contains **inner payloads**, as decimal byte arrays, not full wire
  frames. A button is `[5,0,T,C,S]`; three clean pairs should alternate states
  `0,1,0,1,0,1` on the one expected ID. Preserve any extra edges as bounce
  evidence instead of filtering them away. Ticks are counted separately.
* Wheel payloads are `[5,1,T,0,D]`; interpret `D` as signed (`D-256` if
  `D >= 128`). Check each spaced movement and the signed sum against the four
  counted detents in that direction. Repeating at a faster pace is needed to
  establish coalescing; opposite signs alone do not prove units per detent.

Do not overwrite a failed capture as if it succeeded. The existing CLI has no
raw identity export, no per-event stdout, and no capture-ready event. Stale
session (`suspected_stale_session`) requires an operator replug per the existing
transport contract; no automatic reconnect/retry. These files are offline
evidence, never an app input source or an action stream.

After basic mapping, use separate labeled five-second runs of the same command
for each button's hold, fast double-press, and held-at-entry cases, plus wheel
multi-detent movement. If timing cannot be resolved from these captures, the
finite lab mode below must provide measured capture-boundary synchronization
before claiming acceptance. The initial unknown button state intentionally
drops a first pair until an Up arms it; do not mistake this for missing raw data.

## Can native transport stream? Minimal separate lab path

**The native serial owner can support incremental input; today's executable and
supervisor protocol cannot stream events.** No new low-level transport is
necessary. The relevant existing seams are:

* [`MacosSerialTransport::open/read/write`](../../src/devices/ct/probe/macos_native.rs):
  private capability, lease, exclusive nonblocking descriptor, association
  revalidation, monotonic deadlines, cancellation and at most 50ms read polls.
  Keep the descriptor and lease owned on this worker thread for the whole run.
* [`Engine::next_payload/handle_interleaved`](../../src/devices/ct/probe.rs):
  framing is already incremental. `handle_interleaved` currently appends input
  to `input_samples`; only `run_with_evidence`'s final report exposes it. A typed,
  fallible event sink in the capture phase is the smallest per-event seam.
* [`worker_main/pump/ParentWatchdog`](../../src/native/windows_worker/macos.rs):
  the callback returns one length-prefixed final response, and `pump` rejects
  bytes beyond it. Crucially, the current coordinator **closes child stdin
  after writing the request**. EOF cannot mean parent death in that protocol.
  The watchdog monitors the same-executable coordinator, not fesReview.

A separately reviewed finite **diagnostic-only lab mode** is feasible with the
following concrete changes/tests, leaving `ct-probe` unchanged:

1. Add a private, statically allowlisted input-lab request/scope, not public
   construction of CT authority or reuse of health/HID authority. Reuse the
   existing serial owner and exact discovery checks, then check the agreed
   identity/firmware policy **before** eligible input. Preserve the existing
   production `HardwareUnqualified` refusal.
2. Refactor the shared session engine to call a bounded, fallible typed input
   sink **only after** identity/firmware validation and an explicit fresh capture
   boundary. Suppress query-time input, including buffered suffixes of reads
   completed before that boundary; a later decode timestamp does not make those
   bytes fresh. Finish or reject any old partial frame before arming, within the
   same total/byte budget; never silently reset partial framing to invent a
   boundary. Factor the
   candidate edge reducer to accept decoded events rather than adding a second
   serial decoder or reconstructing wire frames from final report samples.
3. Keep the existing 8000ms total, 1000ms query-I/O, 5000ms capture, 128-event
   and 32768-received-byte ceilings for the first lab; bound new lab output and
   native operations to at most 1000ms and the remaining session budget.
   No budget expansion or
   automatic session renewal is needed to prove per-event delivery. Retain the
   bounded close policy and supervisor cleanup fencing; no patterns/LED/display
   or firmware writes. Normal handshake, queries and safe close still write
   protocol bytes, so “input-only” does not mean zero-write USB.
4. Add a **separate** bounded incremental child/coordinator protocol and pump;
   do not reinterpret diagnostic final JSON as NDJSON. Keep the lab child stdin
   pipe open after its request and monitor EOF independently of serial and
   stdout work. Specify an event/count/byte-bounded nonblocking queue (at most
   128 records for this finite lab), finite per-record write deadline, and
   cancellation on full/stalled output. Sink errors end the session; no drops
   followed by continued dispatch. Parent-death watchdog and hard session budget
   remain active even if stdout blocks. Any readiness/capture-boundary record
   needs an agreed typed lab schema before implementation; this PR adds none.
5. Test the new route synthetically: direct/health/HID/stale-scope entry refused
   before discovery; wrong firmware/identity emits no input; query-time and
   held-at-boundary input never fires; a child event is received **before child
   exit**; stdin EOF with a still-live parent cancels; killing coordinator
   terminates the child group; slow/full stdout cannot prolong session or
   watchdog; serial idle/disconnect/partial frame/cancel terminate correctly;
   ownership is held until close/exit and failed cleanup forbids replacement.
   Keep existing pipe/pty fixture tests and explicit operator contention checks
   distinct from device acceptance.

That lab may output raw diagnostic input for acceptance, not app-dispatchable
actions or a production readiness claim. Before the later on-demand app adapter
ships, fesReview must own foreground eligibility and discard queued input at
focus/child boundaries. The hierarchy is fesReview → controller coordinator →
same-executable contained controller worker, using private pipes; the
coordinator must propagate app-pipe EOF to the worker. Killing only fesReview
must therefore also be tested, independently of the existing worker-parent
watchdog. No server/socket, daemon, global shortcut, OS key injection, arbitrary
command routing, saved configuration, or automatic reconnect is needed.
