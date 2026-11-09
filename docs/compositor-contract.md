# Compositor contract for the browser appliance

What a compositor must implement for the appliance browser, measured rather
than guessed. Tinyland's browser work (campaign item T3) implements the
**required** set; the **optional** set is used when a compositor offers it.

## Evidence

`cargo run -p bctl-proof -- census` boots the reference appliance (Sway with
the pixman renderer, Alpine Chromium 142), starts a second Chromium with
`WAYLAND_DEBUG=1`, and drives it through the B1 interactions (typing, a
`<select>` popup, a click, wheel scrolling, `window.open`) plus a context menu
and an `alert()` dialog. It prints every global Chromium binds and every
request it sends, and keeps the raw log at `.work/tmp/census.log`. Counts
below are from the 2026-11-08 run. Rerun the census when the browser changes;
P2's Helium image will need its own run.

## Findings

- **Buffers are shared memory.** Chromium created 14 `wl_shm_pool` buffers and
  bound no `zwp_linux_dmabuf_v1` (Sway's pixman renderer does not offer it).
  A compositor that offers only `wl_shm` is enough.
- **Menus, `<select>` lists, and tooltips are `xdg_popup`s** placed by
  `xdg_positioner` (anchor rect, anchor, gravity, constraint adjustment,
  size). Chromium sent no `xdg_popup.grab`; dismissal comes from the
  compositor's `popup_done` or from Chromium destroying the popup.
- **`alert()` renders inside the page**, not as a new surface.
- **`window.open` is a second `xdg_toplevel`.**
- **Cursors use `wp_cursor_shape_v1`** (15 `set_shape`); Chromium never called
  `wl_pointer.set_cursor` while the protocol was offered.
- **Frame pacing uses `wp_presentation.feedback`** (92) alongside
  `wl_surface.frame` (92).

## Required

| Interface | Requests Chromium sent |
|---|---|
| `wl_compositor` | `create_surface`, `create_region` |
| `wl_surface` | `attach`, `damage`, `commit`, `frame`, `set_opaque_region`, `set_input_region`, `destroy` |
| `wl_region` | `add`, `destroy` |
| `wl_shm`, `wl_shm_pool`, `wl_buffer` | `create_pool`, `create_buffer`, `resize`, `destroy` |
| `wl_seat` (v5) | `get_pointer`, `get_keyboard`, `release` |
| `wl_pointer` | `release`; events: enter, leave, motion, button, axis, frame |
| `wl_keyboard` | `release`; events: keymap, enter, leave, key, modifiers, repeat_info |
| `wl_output` | `release` |
| `xdg_wm_base` | `get_xdg_surface`, `create_positioner`, `pong` |
| `xdg_surface` | `get_toplevel`, `get_popup`, `set_window_geometry`, `ack_configure` |
| `xdg_toplevel` | `set_title`, `set_app_id`, `set_min_size`, `set_max_size`, `unset_maximized` |
| `xdg_positioner` | `set_size`, `set_anchor_rect`, `set_anchor`, `set_gravity`, `set_constraint_adjustment` |
| `xdg_popup` | `destroy`; events: configure, popup_done |
| `wl_data_device_manager`, `wl_data_device` | `get_data_device`, `release`; events: selection |

Window policy: one primary toplevel fills the output. A second toplevel
(`window.open`) must be handled deterministically, for example by showing it
over the primary and returning focus when it closes. Popups must be placed by
their positioner rules within the output and receive pointer input.

## Optional

Bound and used when offered: `wp_viewporter`, `wp_fractional_scale_manager_v1`,
`wp_presentation`, `wp_cursor_shape_manager_v1` (otherwise Chromium needs
`wl_pointer.set_cursor` with cursor surfaces), `wp_content_type_manager_v1`,
`wp_single_pixel_buffer_manager_v1`, `xdg_activation_v1`,
`zwp_text_input_manager_v3`, `zwp_primary_selection_device_manager_v1`,
`zxdg_decoration_manager_v1` and `org_kde_kwin_server_decoration_manager`,
`zxdg_output_manager_v1`, `zwp_pointer_gestures_v1`, `zwp_tablet_manager_v2`,
`zwp_pointer_constraints_v1`, `zwp_relative_pointer_manager_v1`,
`zwp_idle_inhibit_manager_v1`, `zwp_keyboard_shortcuts_inhibit_manager_v1`,
and `zxdg_exporter`/`zxdg_importer` v1 and v2.

That Chromium runs correctly *without* each optional global is an inference
from it being feature-detected, not yet observed; T3's run of Chromium on
Tinyland, which offers none of them, is the check.

## Tinyland against this contract

Tinyland offers `wl_compositor` 4, `wl_subcompositor`, `wl_shm`, `wl_output` 2,
`wl_seat` 5 (keyboard, plus pointer when an evdev pointer exists),
`wl_data_device_manager` 3, and `xdg_wm_base` 1, and none of the optional
globals. It covers the required set: `xdg_positioner` rules place popups
inside the output, a press outside them sends `popup_done`, toplevels are
maximized with the newest shown and activated, and focus returns to the
previous toplevel when it closes. Cursors use `wl_pointer.set_cursor`: the
compositor draws the client's cursor surface, so it needs no cursor artwork
of its own, which `wp_cursor_shape_v1` would.

`BCTL_PROOF_IMAGE=.work/images/ref-alpine-tinyland.tar cargo run -p
bctl-proof -- m1` runs the B1 interactions on Tinyland, including choosing a
`<select>` option with the pointer and returning focus after `window.open`.
Alpine Chromium ran correctly there without any optional global.
