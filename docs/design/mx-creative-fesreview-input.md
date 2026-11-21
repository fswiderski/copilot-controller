# MX Creative fesReview input adapter

Status: implemented adapter, synthetic tests only. No live hardware
qualification is claimed.

## Authority and lifecycle

`review-input --device mx-creative` is a private, exact child command for
foreground fesReview use. It launches the same executable as a non-detached
process-group child, on the dedicated `__mx-creative-review-worker` route. The
route has no diagnostic allowlist entry and issues a distinct MX Creative review
capability. The ordinary HID diagnostic capability cannot be created in that
scope, and the streaming capability cannot be created by an evidence probe.

The parent retains the worker's stdin. EOF terminates the session and all
process-group resources are killed and reaped. Parent PID is also independently
watched by the worker. An app stdin byte is rejected: the input stream is a
lifecycle channel, not a command channel. Worker setup, internal frames, output
lines, and diagnostic bytes are individually bounded; a blocked output line
terminates the session rather than accumulating actions. There is no daemon,
listener, global hook, foreground query, shortcut, keyboard/consumer output,
injection, HID write, seize, or keymap change.

The two exact IOKit collections from `MX_CREATIVE_CONSOLE.md` must each be
unique. Their full original report descriptor SHA-256, product, VID/PID,
transport, primary usage, sizes, and the keypad USB LocationID are revalidated.
Input is opened with option `0` and wrapped read-only. Opening both devices is
followed by a 50 ms explicit pre-arm drain: reports are repeatedly consumed
for the whole boundary. Dial reports are discarded, while the last keypad state
is retained as the reducer baseline. Thus a button held across arming produces
no post-arm press; an action resumes only after release and a new press.
Intentionally holding a button before the child arms is therefore not replayed.
A fresh invocation has a fresh reducer and therefore cannot replay actions from
a previous foreground generation.

Input Monitoring is still an operator prerequisite. Permission failure, absent
or multiple collections, descriptor or reopen mismatch, parent/supervisor loss,
IPC failure, and output backpressure all fail closed.

## Next block: keypad icons

Physical display or icon output is intentionally not implemented or authorized
in this live input block. A later fesReview-specific image pipeline may render
the already-mapped seven actions as meaningful keypad icons: play/pause, start,
end, back one second, forward one second, screenshot, and dictation. Any such
work must remain a separate block with its own image-format, packaging,
permission, exact-device write, lifecycle, and physical acceptance evidence.
No display write or claim is made here.

## Action reduction

Stdout contains only newline-terminated records of the form
`{"action":"next_frame"}`. No readiness, diagnostics, or device bytes are emitted
to stdout. The first seven accepted keypad press values on report 19,
`values[5]`, map as follows:

| MX keypad | fesReview action |
|---:|---|
| 1 | `toggle_play` |
| 2 | `seek_start` |
| 3 | `seek_end` |
| 4 | `back_one_second` |
| 5 | `forward_one_second` |
| 6 | `screenshot` |
| 7 | `toggle_dictation` |

Only a changed press state is emitted. Release `0` and values 8/9 are silent;
system, consumer, vendor, and other report IDs have no page action.

For the dialpad, only exact normalized standard-mouse report 2 with length 8 is
considered. The observed dial delta is the Consumer AC Pan byte at report index
7, interpreted as signed `i8`; the nominal wheel byte at index 6, coordinates,
buttons, and every other byte are ignored. Negative is `previous_frame`,
positive is `next_frame`, and zero is silent. The first tick
in either direction is emitted immediately. A same-direction stream emits at
most once per 100 ms; suppressed ticks are not queued. A direction reversal
resets the interval and emits immediately. This bounded policy supports
continuous frame stepping at no more than 10 frame actions per second.

The review capability is structurally read-only below the existing
`HidDevice` wrapper: its filtered transport reports `supports_write() == false`
and its backend and transport reject every write or generic open before
dispatch. The review scope does not expose the ordinary write-capable IOKit
transport constructor.

The synthetic suite covers the mapping, releases, unknown controls and reports,
sign handling, coalescing and reversal, duplicate/malformed/oversized child
frames, held-state arming, structural write rejection, output backpressure, and
parent EOF/data rejection. Live HID behavior, permission transitions,
reconnects, and physical cadence still require an explicit hands-on acceptance
pass.
