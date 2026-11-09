# Click fixtures adapted from Playwright

These pages are adapted from Playwright's click tests, which are independent of
bctl, so that bctl's actionability is checked against cases someone else chose.
Playwright is only their source: nothing here runs or links it.

Source: [microsoft/playwright](https://github.com/microsoft/playwright) at
`84e2bdcaa5a6e4a9e8afcbe8b1bca4ff8a8cb69a`, file `tests/page/page-click.spec.ts`
and its assets `tests/assets/input/button.html` and
`tests/assets/input/animating-button.html`. Playwright is licensed under the
Apache License 2.0, a copy of which is in `LICENSE`; its notice is in `NOTICE`.
Each page carries the license header of the file it was adapted from and says
that it was changed.

| Page | Upstream tests | Changed |
|------|----------------|---------|
| `late-layout.html` | CL-042 should wait for stable position; CL-020 should waitFor display:none to be gone; CL-021 should waitFor visibility:hidden to be gone; CL-022 should waitFor visible when parent is hidden; CL-049 should wait for button to be enabled | One page holds every case. The test sets each change off with `arm(id, ms)`, the page makes it on a timer, and the page records each trusted pointer event with its target and time, and the moving button's left edge when clicked. |
| `overlay.html` | CL-043 should wait for becoming hit target | Adds a second overlay that is removed rather than moved. |
| `fractional.html` | CL-007 should click the aligned 1x1 div; CL-008 should click the half-aligned 1x1 div; CL-009 to CL-012 should click the unaligned 1x1 div v1 to v4 | Every div is on one page, each in its own cell, plus two inside a `zoom: 1.37` box; the test repeats them at device scale factors other than 1. |
| `detach.html`, `frame.html` | CL-060 should fail when element detaches after animation; CL-061 should retry when element detaches after animation; CL-069 should not hang when frame is detached | The button really moves: upstream's keyframes name `marign-left`, so its button never animated. Adds a button moved into an iframe's document. |
| `navigation.html`, `landing.html` | CL-032 should click links which cause navigation; CL-067 should not throw protocol error when navigating during the click; CL-068 should retry when navigating during the click; CL-003 should issue clicks in parallel in page and popup | The navigation during the click is the page's own, not a test hook. Adds a button that navigates on press. The pages keep their records in `sessionStorage`, so they survive the navigation. |
| `hang.html` | CL-001 should click the button (its `button.html`) | The button's press handler reports itself with a beacon and then blocks the page, so the reply to the press never comes in time. Upstream has no such test. |
