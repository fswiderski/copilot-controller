# Windows read-only observations

This module is not yet wired into the parent crate. It never grants authority:
successful reports remain read-only, not command-ready, and not qualified.
Executable names are deployment-provided basenames, not installation identities.

## Integration dependencies

- `serde` with `derive`, and `uuid` 1.x.
- `libc` 0.2 on Linux and macOS for the shared native clock.
- On Windows, `windows-sys` 0.61 with `WINDOWS_FEATURES`, and `windows` 0.62
  with `WINDOWS_COM_FEATURES` from `mod.rs`.
- `serde_json` 1.x as a dev-dependency for the contained tests.

The integrator supplies a non-nil runtime epoch, keeps it bound to the same
clock source and boot, and provides exact CLI and native-host basenames via
`ExecutableNames::new`.

## API and boundary

`ordinary_diagnostic` independently samples the clock, current process,
bounded process snapshot, foreground, desktop, and serial-interface metadata.
On Windows it **does enumerate device metadata**; tests must use the pure
selectors and synthetic backends instead. It does not open a serial device.
On unsupported platforms the five Windows sections are explicitly unavailable.

`observe_exact_process` checks the requested lifetime and preserves independent
security, same-image file identity, foreground, and desktop results. Image
comparison opens only the equal image paths for file attributes and reports
unverified provenance. Run explicit probes in a deadline-supervised process:
native calls can block and this module does not enforce a hard deadline.

`observe_ui_structure` and `observe_console_membership` are helper-only. Invoke
them only in a freshly launched, separately supervised disposable process. The
caller must enforce the 12-second deadline, response correlation, exact
target/window identity and lifecycle invalidation. In particular, UI Automation
providers can hang; the MTA thread cannot be cancelled by this module. Console
attachment is process-wide; the helper must own no prior attachment. A missing
console HWND alone does not prove that condition, and `AttachConsole` may still
refuse it. This module does not launch or supervise a worker.

The generic backend traits exist for synthetic testing and integration; their
outputs are not authenticated native evidence. UI counts and console membership
do not establish a pane, tab, presentation, or authorization relationship.
A pseudo-console HWND can be message-only and is not a terminal-pane HWND.
No titles, UI text, console buffers, input, focus actions, device paths, raw
SIDs, logon IDs, usernames, or full image paths are reported.
