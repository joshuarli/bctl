# Job definitions

`bctl.toml` declares reusable browser jobs. A job runs against a managed
browser session; this format only names what should run and with which browser.
Every table rejects unknown fields, and the file must start with
`version = 1`.

Job names and browser names follow the session-name rules: `a-z`, `0-9`, and
`-`, 1 to 40 characters, not starting or ending with `-`.

## Top level

| Field     | Type   | Default | Constraint                                                          |
|-----------|--------|---------|---------------------------------------------------------------------|
| `version` | int    | —       | required, must be `1`                                               |
| `browsers`| table  | empty   | each entry of `[browsers.<name>]`; every browser a job names must exist |
| `jobs`    | table  | empty   | each entry of `[jobs.<name>]`                                       |

## `[browsers.<name>]`

| Field        | Type          | Default      | Constraint                    |
|--------------|---------------|--------------|-------------------------------|
| `image`      | string        | —            | required, non-empty           |
| `viewport`   | `[int, int]`  | `[1280, 800]`| each edge `320..=7680`        |
| `cpus`       | int           | `2`          | `1..=16`                      |
| `memory_mib` | int           | `4096`       | `1024..=65536`                |

## Grab jobs

`kind = "grab"` crawls one or more pages and writes captured output.

| Field         | Type        | Default        | Constraint                                  |
|---------------|-------------|----------------|---------------------------------------------|
| `kind`        | string      | —              | required, `"grab"`                          |
| `browser`     | string      | —              | required, must name a `[browsers]` entry    |
| `profile`     | string      | the job name   | valid profile name                          |
| `urls`        | string[]    | —              | required, non-empty, each `http://`/`https://` |
| `max_pages`   | int         | unset          | non-negative                                |
| `jobs`        | int         | unset          | page workers, `1..=64`                      |
| `robots`      | bool        | `true`         | respect `robots.txt`                        |
| `max_runtime` | duration    | unset          | integer plus `s`, `m`, or `h`               |

## Exec jobs

`kind = "exec"` runs a program on the host, attached to the session's browser
over CDP through `BCTL_CDP_URL`; nothing runs inside the VM. The program has
the user's host authority, the same as running it by hand. There is no shell:
`argv` is passed to the program directly.

| Field     | Type     | Default      | Constraint                                              |
|-----------|----------|--------------|---------------------------------------------------------|
| `kind`    | string   | —            | required, `"exec"`                                      |
| `browser` | string   | —            | required, must name a `[browsers]` entry                |
| `profile` | string   | the job name | valid profile name                                      |
| `argv`    | string[] | —            | required, non-empty, `argv[0]` non-empty                |
| `cwd`     | string   | unset        | relative to the `bctl.toml` directory; no `..`, not absolute |
| `timeout` | duration | unset        | integer plus `s`, `m`, or `h`                           |

## Durations

An integer followed by exactly one unit: `s`, `m`, or `h` — for example `90s`,
`30m`, `2h`. Zero, fractions, multiple units, and empty values are rejected.

## Example

```toml
version = 1

[browsers.default]
image = "ref-alpine-chromium"

[browsers.wide]
image = "ref-alpine-chromium"
viewport = [1920, 1080]
cpus = 8
memory_mib = 8192

[jobs.mirror-docs]
kind = "grab"
browser = "default"
profile = "docs"
urls = ["https://example.com/docs/"]
max_pages = 500
jobs = 4
robots = true
max_runtime = "30m"

[jobs.extract-products]
kind = "exec"
browser = "wide"
argv = ["python3", "jobs/extract.py"]
cwd = "jobs"
timeout = "10m"
```

Resolving `mirror-docs` yields the image `ref-alpine-chromium`, viewport
`[1280, 800]`, `2` CPUs, `4096` MiB, the `docs` profile, its single URL, up to
`500` pages across `4` workers with `robots.txt` respected, and a `30m`
runtime cap.

## Running jobs

The daemon runs jobs; the CLI only resolves and asks. Closing the CLI never
stops a run.

| Command | Effect |
|---------|--------|
| `bctl run JOB --session S [--config FILE]` | Resolves `JOB` from `FILE` (default `./bctl.toml`) and runs it in session `S`. |
| `bctl grab URL... --session S` | Runs a one-off grab of the URLs, respecting `robots.txt`, in the existing session `S` as it is. |
| `bctl runs [RUN] [--session S]` | Lists runs, newest first, or shows one run with every resolved field. |
| `bctl logs RUN` | Prints the run's log. |
| `bctl pause RUN` | Stops a grab at its next page boundary, keeping its state. |
| `bctl resume RUN` | Continues a grab that waits for an operator, is paused, or was interrupted. |
| `bctl cancel RUN` | Ends a live or paused run for good. |

Every command takes `--json` except `logs`. Errors carry stable codes:
`NO_SUCH_RUN`, `RUN_ACTIVE`, `NOT_PAUSABLE`, `NOT_RESUMABLE`, `NOT_LIVE`, and
`SESSION_NOT_READY`, besides the session codes.

`bctl run` brings `S` up with the job's profile and the resources its
`[browsers.*]` entry resolves to (image, viewport, CPUs, memory). If `S` exists
with any of them different, the run fails with `CONFLICT` and nothing starts.
A session runs one run at a time; a second fails with `RUN_ACTIVE`.

### Exec runs

The program runs on the host as its own process group, with no shell, in
`cwd` (the `bctl.toml` directory when unset). Its environment adds:

| Variable | Value |
|----------|-------|
| `BCTL_CDP_URL` | A CDP control URL for the session, revoked when the attempt ends |
| `BCTL_OUTPUT_DIR` | The run's output directory, which the program may fill |
| `BCTL_RUN` | The run id |

Standard input is empty. `timeout` and `bctl cancel` send the whole process
group `SIGTERM`, then `SIGKILL` 5 seconds later. When the program exits, any
process left in its group is killed. An exec run cannot be paused or resumed.

While the program holds a socket open through `BCTL_CDP_URL`, it is the
session's external controller (see [automation](automation.md#external-controllers)):
other CDP URLs and dashboard takeovers are refused until it closes. Its own
`bctl page` calls still run.

#### browser-use and agent-browser

`tests/interop/recipes/` holds an exec program for each of the two external
clients bctl is tested with: browser-use 1.0.0 and agent-browser 0.42.0. Both
run on the host with the user's authority, and attach only through
`BCTL_CDP_URL`. Each writes `result.json` to `BCTL_OUTPUT_DIR` with `run`,
`success`, `result` and `errors`, and exits 0 only on success. Both remove the
capability URL from everything they print or write, because each client puts
it in its connection errors.

```toml
[jobs.research]
kind = "exec"
browser = "default"
argv = ["python3", "browser_use_job.py", "--model", "anthropic:claude-sonnet-5-5", "Find the opening hours"]
timeout = "10m"
```

- **browser-use** (`browser_use_job.py`) runs one `Agent` with
  `Browser(cdp_url=BCTL_CDP_URL)`. `--model` is required: browser-use's
  default model is Browser Use's paid gateway. Its plain `Browser()` is a
  cloud browser, which the recipe never uses. An unreachable URL fails the job
  and starts no other browser. `Agent.run()` reports a refused attach as a
  failed history rather than raising, so the recipe checks
  `is_successful()`.
- **agent-browser** (`agent_browser_job.py`) gives a Python program an
  `AgentBrowser` that runs CLI commands in the client session
  `bctl-run-BCTL_RUN`, with the URL in `AGENT_BROWSER_CDP` rather than in any
  argv. It opens its own tab first, because an attached client otherwise
  drives whichever tab it finds, possibly a hidden one where clicks are
  dropped. agent-browser's daemon leaves the run's process group, so the
  recipe closes it on exit and on `SIGTERM`. An unreachable URL fails the job
  and starts no local Chrome.

On SmolVM, a job also gets `BCTL_FILES_DIR`, the session's files directory
on the host, and `BCTL_GUEST_FILES_DIR`, where the guest sees it. A client
uploads a file there by its guest path and finds the browser's downloads
there; see [files](files.md).

Closing either client leaves the session `ready`; only `bctl down` stops the
browser. A cancelled, timed-out or interrupted attempt is not retried, so an
action the client had already performed is not performed again. Run
`tests/interop/run.sh` to check both recipes against a real session.

### Grab runs

bctl runs Shadowdriver's Grab, `BCTL_GRAB` or `grab` from `PATH` as the daemon
found it, attached to the session's browser through a control URL. Grab writes
its mirror under the run's output directory and reports progress on its event
stream. When Grab meets a challenge or a login wall, the run moves to
`needs-interaction` and names the URL; act in the session's browser (the
dashboard or any CDP client), then `bctl resume RUN`. The crawl continues in
the same browser and profile.

Grab keeps its control URL's sockets open while it waits, so it stays the
session's external controller. For that wait only, it stands aside: one other
CDP URL (`bctl cdp-url SESSION`) can attach beside it, and a dashboard takeover
is not refused because of Grab's sockets. Grab itself is unchanged and sends
nothing while it waits. Outside `needs-interaction` the one-controller rule of
[automation](automation.md#external-controllers) holds. Resuming the run, with
`bctl resume` or the dashboard's "Resume automation", closes the operator's
sockets within about a second and refuses that URL again while the crawl runs.
It can attach again the next time the run needs interaction. A run that
ends while it waits (`bctl cancel`, `bctl pause`) leaves the operator's
sockets open, and the operator then holds the session as any controller does.

`bctl pause` asks Grab to stop at its next page boundary; the run is `paused`
and the VM keeps running. `bctl resume` of a paused or interrupted grab starts
a new attempt over the same output directory, which continues from Grab's
`state.db`. `bctl cancel` asks Grab to stop at a page boundary and ends the
run; a Grab that has not stopped 60 seconds later is sent `SIGTERM`.

A crawl Grab reports as succeeded counts as a success only after `grab verify`
replays every mirror it wrote. Cancellation during verification currently sends
TERM without a KILL deadline, so an unresponsive verifier can keep the attempt
live; [BCTL-6](../TODO.md#bctl-6--p2--cancellation-can-leave-grab-verification-running-indefinitely)
tracks that gap.

### In the dashboard

A session's page lists its runs. A run's page shows its inputs, resolved
config, progress, attempts, outcome, a log tail that follows the run, and its
output files. Files are downloads only: they are served as an opaque
attachment under a `sandbox` policy, never as a page on the dashboard origin.

When a run is `needs-interaction`, the page embeds the session's live view.
"Take control" moves the session's authority to `yielding` and then, once
bctl's own native commands in flight have ended, to `human`: only then does
that viewer's keyboard and pointer reach the browser. From `yielding` on, new
CDP attachments and native commands that change the page are refused. Grab's
open sockets do not refuse the takeover, but an operator's CDP URL that is
attached does: close it first. Grab leaves no tab open at a login wall, so
open the named URL in the browser yourself. "Resume automation" hands control back to automation and sends
`resume`, as `bctl resume` does. If another viewer has control, it refuses.
The states are described in [automation.md](automation.md#authority).

### States and outcomes

A run's state is its current attempt's: `starting`, `running`,
`needs-interaction`, `paused`, `succeeded`, `failed`, `cancelled`, or
`interrupted`. An ended attempt carries one outcome:

| Outcome | Meaning |
|---------|---------|
| `succeeded` | The program exited 0, or Grab finished and its mirror verified |
| `browser-crashed` | The run failed and the browser no longer answers CDP |
| `site-rejected` | The crawl ended still blocked by a challenge |
| `login-required` | The crawl ended still waiting for a login |
| `verification-failed` | Grab finished, but `grab verify` rejected the mirror |
| `job-error` | The program failed, timed out, or could not start |
| `cancelled` | `bctl cancel`, or `bctl down` of the session |
| `interrupted` | bctl stopped while the attempt was live |

When the daemon restarts, every attempt it was running ends `interrupted`. A
process of that attempt still running is sent `SIGTERM`, and `SIGKILL` 15
seconds later. bctl never retries an interrupted attempt on its own.

### Run records and files

`bctl runs RUN --json` shows the resolved config, its `input_digest` (SHA-256
of that config), the profile, the image and its manifest digest
(`image_digest`), the browser version, and every attempt. A run's files live in
`$BCTL_HOME/runs/RUN/`: `output/`, the `log`, and each attempt's event stream
and control channel. The log holds every attempt's output and bctl's own
notes, up to 8 MiB per run; later output is discarded.

### Private addresses

Guests reach the public internet only. `bctl up S --allow-private CIDR`
(repeatable) lets session `S` reach a private IPv4 range too, and is part of
its configuration. With SmolVM's default guest link, `100.96.0.0/30` allows
the host's loopback, which the guest reaches at `100.96.0.1`.
Reach a host service there rather than at the host's LAN address: SmolVM
dials a LAN address as an ordinary host connection, which a macOS content
filter can stall mid-response, while it dials `100.96.0.1` as `127.0.0.1`.
