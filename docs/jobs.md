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

`kind = "exec"` runs a program inside the session. There is no shell: `argv` is
passed to the program directly.

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
