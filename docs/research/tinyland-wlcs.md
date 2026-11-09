# Driving tinyland through WLCS

**Verdict: feasible, and done.** WLCS (`canonical/wlcs`) drives tinyland through
its `wlcs_server_integration` ABI with one library split, one test-only feature,
and one separate cdylib crate (about 300 lines). No compositor behavior changed
to make the adapter work. The suite is a weak fit for tinyland's kiosk policy,
though: of 1228 tests, 79 pass, 767 skip as unsupported protocols, and 379 fail,
and almost all of those failures come from what WLCS assumes of a compositor,
not from tinyland defects. The run did find one real bug (fixed below).

WLCS is GPL-3 (test files are GPL-3, some build files GPL-2+ or Expat). It is
used only as a test runner in a container. It is never linked into tinyland or
the guest image, and the adapter is not shipped.

## What the ABI needs, and what tinyland had

| WLCS hook | Needs | Tinyland |
|---|---|---|
| `create_server` / `start` / `stop` | A compositor that runs off a thread, headless | `server::run` needs DRM, seatd, and evdev, and owns the loop. New: `server::headless::Headless` (feature `headless`) builds the same `State` and globals with no output, and `pump()` dispatches, composes, and fires frame callbacks. The adapter runs it on its own thread. |
| `create_client_socket` | A connected socket fd | New: `Headless::connect` inserts one end of a `socketpair` as a client. |
| `create_pointer` (`move_absolute`, `move_relative`, `button_up`, `button_down`) | Programmatic pointer injection | `State::pointer_frame` already took `PointerFrame`; `Headless` wraps it. Each call is synchronous, so a client round trip sees the events. |
| `position_window_absolute` | Windows at chosen coordinates | None, by design: every toplevel is maximized at the output origin. The hook is a no-op. |
| `create_touch` | Touch injection | None: the seat has no touch. A stub keeps WLCS from dereferencing null. |
| `get_descriptor` | The claimed globals and versions | Tinyland's own: `wl_compositor` 4, `wl_subcompositor` 1, `wl_shm` 1, `wl_output` 2, `wl_seat` 5, `wl_data_device_manager` 3, `xdg_wm_base` 1. WLCS skips tests for anything else, which is how unsupported protocols stay apart from failures. |

The ABI has no keyboard hook. WLCS cannot test key text, modifiers, repeat, or
key release, so those regressions live in tinyland's own suite.

The one structural change is a `lib.rs` next to `main.rs` so the adapter can
link the compositor code; `create_globals` is shared between `run` and
`Headless` so the two cannot drift. `main.rs` only imports from the library.

Build requirements: C++20, CMake, Boost headers, GoogleTest and GoogleMock,
`libwayland-client` and `libwayland-server`. All of these exist on Alpine, so
the runner and the adapter are both musl (`-C target-feature=-crt-static` for
the cdylib). Nothing needs glibc. `harness/wlcs/Dockerfile` pins WLCS to
`2b385328366de5db19521b9b3eef81861430d984` (v1.8.1) on the pinned Alpine base.
WLCS must be built as `Debug`: its `test_truncated_shm_file` calls
`ftruncate` inside `assert`, so a `Release` build never truncates the file.

## Running it

```sh
guest/tinyland/harness/wlcs/run.sh --gtest_list_tests
guest/tinyland/harness/wlcs/run.sh --gtest_filter='XdgSurfaceStableTest.*'
guest/tinyland/harness/wlcs/run.sh --sweep
```

`--sweep` runs every test in its own process (20 second limit) and prints
`PASS`, `FAIL`, `SKIP`, `CRASH`, `TIMEOUT`, or `DISABLED` per test. It takes
about 2.5 minutes on this Mac, including a cached image build. Per-test output
lands in `.work/build/wlcs/target/sweep/`.

## Results

Run on 2026-11-09, arm64 Alpine 3.22 container, tinyland at the commit that
adds this document.

| Outcome | Tests |
|---|---|
| Pass | 79 |
| Skip: protocol or version unsupported (WLCS reports "missing extension") | 767 |
| Fail | 379 |
| Disabled upstream | 3 |
| Crash or timeout | 0 |

### Unsupported protocols (skips)

Not claimed by tinyland, so they are not failures: wlr-layer-shell (316 tests),
wlr-foreign-toplevel-management, wlr-virtual-pointer, `wp_viewporter`,
`wp_fractional_scale_v1`, `zwp_pointer_constraints`, `zwp_relative_pointer`,
text-input v2/v3 with input-method, primary selection (both flavors),
`zxdg_decoration_manager_v1`, `zxdg_output_manager_v1`, and `zxdg_shell_v6`. Also skipped are tests that need a newer `xdg_wm_base` than 1.

### Passes in claimed protocols

Core surface, output, frame callbacks, xdg-shell, and a few subsurface and
data-device cases: surface enters output, frame timestamps increase,
`wl_output` properties, frame requests without a buffer, one frame at a time,
a client lying about buffer size, xdg_shell configure, role-conflict error,
parent set and cleared, subsurface parent and placement, and the copy-paste
offer after a sink gains focus. Nine more are WLCS self tests.

### Failures in claimed protocols, by cause

The cause column comes from each test's first failure line; the split between
the first two rows is a text match, not a hand audit, so treat counts as
approximate.

| Cause | Tests | Tinyland defect? |
|---|---|---|
| The test places a window and then hits it with the pointer (`position_window_absolute`); tinyland maximizes everything | 102 | No. Kiosk policy. Includes all surface input-region tests and the pointer-crossing tests. |
| WLCS never sends `xdg_surface.ack_configure` and commits a buffer straight after `get_toplevel` or `get_popup`; tinyland rejects with `unconfigured_buffer` | 251 | No. The spec requires the ack, and wlroots and Weston enforce it. This hides every WLCS popup-placement test (positioner anchor, gravity, constraint adjustment, grabbed popups). |
| Touch | 14 | No. No touch capability claimed. |
| Other | 12 | See below. |

The 12 others:

- `BadBufferTest.test_truncated_shm_file`: tinyland raises `wl_shm.invalid_fd`
  on `wl_shm` when the pool is created over a truncated file; the test wants
  the error on `wl_buffer`. Same code, earlier and on a different object.
- `XdgSurfaceStableTest.creating_xdg_surface_from_wl_surface_with_attached_buffer_is_an_error`
  and `..._committed_buffer_is_an_error`: expect
  `xdg_wm_base.invalid_surface_state`, which exists from version 6 only.
- `XdgSurfaceStableTest.attaching_buffer_to_unconfigured_xdg_surface_is_an_error`:
  expects the error at `attach`; tinyland raises it at `commit`, as Weston and
  wlroots do.
- `XdgToplevelStableTest.when_parent_is_set_to_self_error_is_raised` and
  `..._to_child_descendant_...`: expect `invalid_parent`, also a later
  version.
- `XdgToplevelStableConfigurationTest.defaults`, `window_can_maximize_itself`,
  `activated_state_follows_pointer`: tinyland always configures maximized at
  the output size and activates by stacking, not by pointer.
- `FrameSubmission.test_buffer_can_be_deleted_after_attached`: destroying an
  attached buffer before commit disconnects the client with a `wl_surface`
  error code 0 (`invalid_scale`). The spec leaves the result unspecified, so
  the disconnect is defensible, but the code is a misuse of the enum. Left
  alone.
- `FrameSubmission.when_client_endlessly_requests_frame_then_callbacks_are_throttled`:
  the headless pump fires callbacks at once, where real scanout waits for a
  flip. Harness behavior, not tinyland's.
- `CopyCutPaste.given_sink_has_focus_when_source_makes_offer_sink_sees_offer`:
  the test picks keyboard focus by clicking a positioned window.

### Real bug found and fixed

A shm buffer whose file shrinks after the pool was created (a client crash or
`ftruncate`) disconnected the client with `wl_surface` error 0
(`invalid_scale`) at commit. It now raises `wl_shm.invalid_fd` on the
`wl_buffer`, the code the protocol defines for this and the object Weston
uses. Regression test:
`committing_a_buffer_whose_file_was_truncated_reports_invalid_fd_on_the_buffer`.

Several other client-limit errors (too many callbacks, memory budget) also
post code 0 on `wl_surface`. They have no protocol-defined code, so they were
left as they are.

## Input regressions in tinyland's own suite

WLCS cannot cover these, so they are tests in `src/input.rs` and
`src/server/tests.rs`, none of which found a defect:

- shifted punctuation (`!`, `:`, `"`, `_`, `+`, `>`, `?`);
- modifier chords: each held modifier stays reported until its own release;
- fractional pointer motion reaches the client as `wl_fixed`;
- input after the focused surface is destroyed reaches nobody and does not
  panic;
- a modifier held when focus is lost is reported to the next focus and cleared
  there.

Already covered elsewhere: popup placement and outside-click dismissal and
second-toplevel focus return (`server/tests.rs`), modifier reset on seat
resume and unplug (`input.rs`, `seat.rs`), and repeat, compose, and text
(`bctl-proof -- m1`, `kbvm_oracle`).

## What this does not show

- Popup grab and focus are not exercised by WLCS here. Every WLCS popup test
  needs the client to ack configures, which WLCS does not do. Running them
  needs a WLCS patch or an adapter-side option to relax the check, which
  would test a different compositor than the one that ships.
- Key release on disconnect and on seat takeover: the compositor resets
  modifiers on seat resume and unplug; the cases where a client disconnects
  with keys held rely on keyboard state belonging to the compositor, covered
  by the focus-loss test above. No test drives a real VT takeover.
- B1 (`bctl-proof -- m1`) was not rerun; the only behavior change is the error
  object and code for a truncated buffer.
