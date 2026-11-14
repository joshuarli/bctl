# agent-browser provider

`bctl agent-browser-plugin` is a `browser.provider` plugin for agent-browser
0.43.0. With it, `agent-browser --provider bctl open URL` gets its browser
from a bctl session instead of launching Chrome. Plain `--cdp` attachment
with a URL from `bctl cdp-url` works without it.

## agent-browser 0.43 baseline

`tests/interop/run.sh` pins upstream **v0.43.0** at commit
`1cc19526f3641c8543d55aa6700e7c55b37ac356` and downloads the
host-specific native binary using the release's SHA-256. The installed
`usr` binary comes from `sh sync tools agent-browser`, which also installs
the matching upstream `skill-data` resources and verifies `skills get core`.
These are separate locations: the interop harness tests a pinned binary;
the host's `usr` installer follows the latest verified release (minimum 0.43).

The 0.43 release adds hints for otherwise ambiguous snapshot controls,
faster large-page snapshots, immediate errors after a tab closes,
high-density recording repair, and a `modern-web-guidance` skill. Their
availability in the CLI does not prove VM-specific behavior until the
interop suite is rerun on that host/runtime.

## Native WebMCP integration

agent-browser 0.43 already implements **CDP WebMCP** discovery and
invocation. With the bctl provider configured as below, these operate in
the browser attached through bctl:

```sh
agent-browser --provider bctl open https://example.com
agent-browser webmcp list --json
agent-browser webmcp list TOOL --frame FRAME_ID --json
agent-browser webmcp invoke TOOL --frame FRAME_ID --params '{"query":"example"}'
```

The example tool and arguments are placeholders; fetch actual page schemas
before invoking. `agent-browser skills get webmcp-gen` provides a browser
tool-generation/evaluation workflow. Chrome WebMCP is experimental;
**bctl's current Helium images have not been qualified for the CDP WebMCP
domain**. An attached browser may return `webmcp_unsupported`. Do not
globally enable Chromium experiments or weaken the VM sandbox as a shortcut.
The existing `bctl webmcp` JavaScript adapter is independent and likewise
unqualified; [BCTL-23](../TODO.md) tracks the compatibility comparison.

Use one agent-browser controller for a given bctl session, and close it
before a competing client attaches. Follow the caller's confirmation policy
for mutating/consequential site tools regardless of page-provided annotations.
## Configure it

agent-browser runs plugins it is told about and installs nothing. Add bctl to
`~/.agent-browser/config.json`, or pass the same array in
`AGENT_BROWSER_PLUGINS`:

```json
{
  "plugins": [
    {
      "name": "bctl",
      "command": "/absolute/path/to/bctl",
      "args": ["agent-browser-plugin"],
      "capabilities": ["browser.provider"]
    }
  ]
}
```

The plugin reaches the daemon of the `BCTL_HOME` it inherits, starting it
if needed.

## Leases

The `args` choose who owns the session:

| `args` | Launch | Close |
|--------|--------|-------|
| `["agent-browser-plugin"]`, optionally with `--image IMAGE` and `--allow-private CIDR` | starts session `ab-<agent-browser session>` | stops and removes that session |
| `["agent-browser-plugin", "--session", "NAME"]` | uses the ready session `NAME` | revokes only the URL it minted; `NAME` keeps running |

**Owned** sessions take their profile from the session name, so
`ab-default` keeps its cookies across launches like any other profile. The
plugin keeps an owner record in `<BCTL_HOME>/agent-browser/` with the
session's generation and URL. Close removes the session only if both match
the cleanup handle agent-browser sends back, so a stale or forged handle
never stops another session. A repeated close does nothing.

**Reclaiming:** when agent-browser dies without closing, its session keeps
running. The next launch for the same agent-browser session stops it first,
then starts a fresh one. If nothing launches again, `bctl down` and `bctl rm`
it by hand. A session named `ab-...` that the plugin did not start is never
stopped: launch fails and names `--session` as the way to use it.

A launch that fails part-way removes the session it started.

## Launch options

agent-browser sends `headed`, `engine`, `userAgent` and `colorScheme`. The
plugin refuses an option whose effect it cannot give:

- `engine` other than `chrome`: sessions run Helium.
- Any `userAgent` or `colorScheme`.
- `headed: true` on Nanos, whose sessions are headless. SmolVM sessions are
  always headed, whatever `headed` says.

## Limits from agent-browser

- agent-browser allows 60 s for a launch and 15 s for a close. On the Mac
  with SmolVM a launch took about 2 s; other hosts are unmeasured.
- A plugin result cannot carry headers, so the URL is a capability URL:
  anyone holding it controls the browser until it is revoked.
- agent-browser hides the plugin's error text from its own errors. Run
  the plugin by hand to read it:

  ```sh
  echo '{"protocol":"agent-browser.plugin.v1","type":"browser.launch","capability":"browser.provider","request":{"session":"x"}}' \
    | bctl agent-browser-plugin
  ```
