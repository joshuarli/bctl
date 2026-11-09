# Driving tinyland through WLCS

**Verdict: feasible, and done.** WLCS (`canonical/wlcs`) drives tinyland through
its `wlcs_server_integration` ABI with one library split, one test-only feature,
and one separate cdylib crate (about 300 lines). No compositor behavior changed
to make the adapter work. The suite is a weak fit for tinyland's kiosk policy,
though: of 1228 tests, 163 pass, 770 skip as unsupported protocols, and 292 fail,
and almost all of those failures come from what WLCS assumes of a compositor,
not from tinyland defects. The runs found one real bug, one misuse of error
codes, and one disconnect on an unspecified case (all fixed below), and one
unimplemented request (popup grab, a known gap).

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
`2b385328366de5db19521b9b3eef81861430d984` (v1.8.1) on the pinned Alpine base
and applies `harness/wlcs/ack-initial-configure.patch` before building (see
below).
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

## The WLCS client patch

WLCS's stable xdg-shell test clients commit a buffer straight after
`get_toplevel` or `get_popup`. xdg-shell requires an initial commit without a
buffer, then an ack of the first `xdg_surface.configure`, and only then a
buffer. Tinyland enforces that (`xdg_surface.unconfigured_buffer`), as wlroots
and Weston do, so the unpatched suite hid every popup test. The patch
(`harness/wlcs/ack-initial-configure.patch`, GPL-3 like WLCS, test-only, never
shipped) adds `XdgSurfaceStable::commit_and_ack_initial_configure` and calls
it before the first buffer. It touches five upstream files:

- `include/xdg_shell_stable.h`, `src/xdg_shell_stable.cpp`: the helper.
- `src/surface_builder.cpp`: `XdgStableSurfaceBuilder::build`, used by the
  input-region and subsurface tests.
- `tests/xdg_popup.cpp`: the popup manager's parent toplevel, in
  `wait_for_frame_to_render`.
- `tests/xdg_toplevel_stable.cpp`: the five tests that attach a buffer to a
  bare toplevel (interactive move and resize, touch steal).

Not patched: eight `SurfaceInputCombinations` tests
(`input_seen_after_surface_unmapped_and_remapped`,
`input_seen_by_subsurface_after_parent_unmapped_and_remapped`) unmap with a
null buffer and attach a buffer again without a new initial commit and
configure. xdg-shell says an unmapped toplevel returns to its state right
after `get_toplevel`, so tinyland is right to reject it. Fixing it needs a
per-surface-type change in WLCS's generic surface code.

## Results

Run on 2026-11-09, arm64 Alpine 3.22 container, tinyland at the commit that
updates this document.

| Outcome | Unpatched | Patched |
|---|---|---|
| Pass | 79 | 163 |
| Skip: protocol or version unsupported (WLCS reports "missing extension") | 767 | 770 |
| Fail | 379 | 292 |
| Disabled upstream | 3 | 3 |
| Crash or timeout | 0 | 0 |

The patch turned 83 failures into passes (84 with the buffer fix below) (popup placement, anchor, gravity,
and dismissal cases, plus input and subsurface cases that need no window
placement). Three tests that used to fail now skip.

### Unsupported protocols (skips)

Not claimed by tinyland, so they are not failures: wlr-layer-shell (316 tests),
wlr-foreign-toplevel-management, wlr-virtual-pointer, `wp_viewporter`,
`wp_fractional_scale_v1`, `zwp_pointer_constraints`, `zwp_relative_pointer`,
text-input v2/v3 with input-method, primary selection (both flavors),
`zxdg_decoration_manager_v1`, `zxdg_output_manager_v1`, and `zxdg_shell_v6`. Also skipped are tests that need a newer `xdg_wm_base` than 1.

### Failures in claimed protocols, by cause

The split comes from each test's output and name, matched by text, so treat
the touch and position counts as approximate; the named rows are exact.

| Cause | Tests | Tinyland defect? |
|---|---|---|
| The test places a window (`position_window_absolute`, parent position) and then hits it with the pointer or expects popup placement relative to that position; tinyland maximizes every toplevel at the origin | about 182 | No. Policy mismatch. Input-region and surface-input combinations, the 44 popup positioner and constraint-adjustment cases (the parent is moved into the middle or off an edge, so slide, flip, and resize outcomes differ), subsurface and multilevel input, pointer motion, surface-moves-under-pointer, window-geometry offset, interactive move and resize (`xdg_toplevel.move` and `resize` are ignored), and two popup pointer-focus tests. |
| The toplevel configuration tests: tinyland always configures maximized at the output size, ignores `set_maximized`, `unset_maximized`, `set_fullscreen`, and `unset_fullscreen`, and activates by stacking, not by pointer | 6 | No. Policy mismatch. |
| Touch | about 93 (12 `TouchTest`, plus touch-parameterized input cases and two toplevel touch tests) | No. No touch capability claimed. |
| Unmap then re-attach without a new initial commit (WLCS violates xdg-shell) | 8 | No. WLCS bug, left unpatched. |
| `xdg_popup.grab` does not move keyboard focus to the popup (`grabbed_popup_gets_keyboard_focus`) | 1 | Known gap, left as is: Chromium sends no grab (`docs/compositor-contract.md`). Tinyland accepts the request and ignores it; outside-click dismissal works without it. |
| Named cases, below | 11 | See below. |

The named cases:

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
- `FrameSubmission.when_client_endlessly_requests_frame_then_callbacks_are_throttled`:
  the headless pump fires callbacks at once, where real scanout waits for a
  flip. Harness behavior, not tinyland's.
- `CopyCutPaste.given_sink_has_focus_when_source_makes_offer_sink_sees_offer`:
  the test picks keyboard focus by clicking a positioned window (policy).

### Real bug found and fixed

A shm buffer whose file shrinks after the pool was created (a client crash or
`ftruncate`) disconnected the client with `wl_surface` error 0
(`invalid_scale`) at commit. It now raises `wl_shm.invalid_fd` on the
`wl_buffer`, the code the protocol defines for this and the object Weston
uses. Regression test:
`committing_a_buffer_whose_file_was_truncated_reports_invalid_fd_on_the_buffer`.

### Error codes for client limits

Every per-client limit (surfaces, regions, region rectangles, frame callbacks,
pools, buffers, outputs, seats, data sources and devices, mime types,
toplevels, memory budget) and the oversized-buffer check posted code 0 on
whichever object hit it. A client reads that as the first error of that
interface: `invalid_scale` on `wl_surface`, `invalid_format` on `wl_shm`,
`missing_capability` on `wl_seat`. They now post `wl_display.no_memory` on the
display object. An internal unknown-pool lookup posts
`wl_display.implementation`. Interface-specific codes that were
already correct are unchanged. The `wayland-server` API cannot name the
`wl_display` object, so `src/server/display_error.rs` looks it up by protocol
id 1 with a local `Interface` value named `wl_display`; the pure-Rust backend
matches interfaces by name. Regression tests assert the object and code on
the wire: `callback_bound_disconnects_offender_only`
and `per_client_memory_budget_rejects_without_harming_healthy_client`.

### Buffer destroyed after attach

Committing a buffer the client destroyed after `wl_surface.attach` used to
disconnect the client. The protocol leaves the result unspecified; Weston
commits a null buffer and wlroots keeps the content. Tinyland now follows
wlroots: the commit behaves as if nothing was attached, the surface keeps its
current pixels, and frame callbacks still queue. WLCS's
`test_buffer_can_be_deleted_after_attached` passes. Regression test:
`committing_a_buffer_destroyed_after_attach_keeps_the_current_content`.

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

- Popup grab semantics: `xdg_popup.grab` is accepted and ignored, and the one
  WLCS test of it fails. Nested grabbed popups and outside-click dismissal
  pass. Popup placement against a parent that WLCS moves is a policy
  mismatch, so constraint adjustment is only checked for a parent at the
  origin.
- Key release on disconnect and on seat takeover: the compositor resets
  modifiers on seat resume and unplug; the cases where a client disconnects
  with keys held rely on keyboard state belonging to the compositor, covered
  by the focus-loss test above. No test drives a real VT takeover.
- Verification of the error-code change: tinyland's arm64 tests (75 library
  tests plus the rest), T0 (`smolvm-proof.sh`), and B1 (`bctl-proof -- m1
  --runs 2` on `ref-alpine-tinyland`, both runs ok). None exercises a client
  limit, so the new codes are covered by the unit tests only.
