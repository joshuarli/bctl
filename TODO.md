Prefix: BCTL
Next: 15

### BCTL-10 · P3 · The web UI has no tone for paused sessions

- **Area:** Types
- **Where:** `web/src/api.ts:7`, `web/src/app.tsx:9`
- **Problem:** The daemon reports `paused` sessions (`crates/bctl/src/daemon.rs:806`), but `SessionView["state"]` omits `paused`, so the type is wrong and `stateTone` returns undefined for a paused session's badge.
- **Recommendation:** Add `paused` to the state union and a tone for it in `stateTone`, and check the badge renders for a paused session.
- **Fix with:** none
- **Effort:** S (hours)
