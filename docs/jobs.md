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

### Grab runs

bctl runs Shadowdriver's Grab, `BCTL_GRAB` or `grab` from `PATH` as the daemon
found it, attached to the session's browser through a control URL. Grab writes
its mirror under the run's output directory and reports progress on its event
stream. When Grab meets a challenge or a login wall, the run moves to
`needs-interaction` and names the URL; act in the session's browser (the
dashboard or any CDP client), then `bctl resume RUN`. The crawl continues in
the same browser and profile.

`bctl pause` asks Grab to stop at its next page boundary; the run is `paused`
and the VM keeps running. `bctl resume` of a paused or interrupted grab starts
a new attempt over the same output directory, which continues from Grab's
`state.db`. `bctl cancel` asks Grab to stop at a page boundary and ends the
run; a Grab that has not stopped 60 seconds later is sent `SIGTERM`.

A crawl Grab reports as succeeded counts as a success only after `grab verify`
replays every mirror it wrote.

### In the dashboard

A session's page lists its runs. A run's page shows its inputs, resolved
config, progress, attempts, outcome, a log tail that follows the run, and its
output files. Files are downloads only: they are served as an opaque
attachment under a `sandbox` policy, never as a page on the dashboard origin.

When a run is `needs-interaction`, the page embeds the session's live view.
"Take control" moves the session's authority to `yielding` and then, once
bctl's own native commands in flight have ended, to `human`: only then does
that viewer's keyboard and pointer reach the browser. From `yielding` on, new
CDP attachments and native commands that change the page are refused. Grab
leaves no tab open at a login wall, so open the named URL in the browser
yourself. "Resume automation" hands control back to automation and sends
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

## External client jobs (planned M8C)

Existing `kind = "exec"` runs are the intended host-side integration
point for browser-use 1.0.0 and agent-browser. The daemon supplies
`BCTL_CDP_URL` (a sensitive capability), `BCTL_OUTPUT_DIR` and `BCTL_RUN`
and starts the program **on the host as the invoking user**, not inside
the VM. This is not a sandbox for model-written Python or an agent-browser
process. No new job kind is proposed.

The browser-use 1.0 attachment must construct
`Browser(cdp_url=os.environ["BCTL_CDP_URL"])` and supply it to `Agent`.
Bare `Browser()` provisions Browser Use Cloud in 1.0 and is **not**
a safe equivalent. agent-browser should use its explicit `--cdp`
attachment and a unique client-side session per run. The precise
released-client command lines and endpoint host resolution remain
unqualified; do not advertise examples as working until the M8C E1 tests
pass. See [source-pinned research](research/external-clients.md).

Client shutdown closes its own CDP connection/harness; `bctl down` remains
an explicit browser lifecycle operation. The current run process-group
cleanup does not yet establish that every detached external client daemon
is reclaimed. Similarly, files downloaded **inside the guest** do not
automatically appear in `BCTL_OUTPUT_DIR`. E2 and E4 define the required
ownership and artifact gates. No external run can claim cooperative
pause or dashboard takeover until proven.
