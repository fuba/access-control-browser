# Architecture

```
+-------------------+        +-------------------+
| Human Operator    |        | LLM Agent         |
| (browser at /)    |        | (acb-cli)         |
+--------+----------+        +---------+---------+
         |                              |
         | HTTP+token                   | HTTP+token
         |                              |
         v                              v
+---------------------------------------------------+
|              acb-daemon (Rust + axum)             |
|                                                   |
|  /healthz                /config                  |
|  /sessions, /sessions/:id/{open,snapshot,click..} |
|  /events (SSE)           /admin/reload            |
|  /  (embedded Next.js UI via rust-embed)          |
|                                                   |
|  +-------------------+   +---------------------+  |
|  | request_policy    |   | element_policy      |  |
|  | (top-level vs sub | + | (subtree-allowed    |  |
|  |  vs inherit_page) |   |  CSS classes)       |  |
|  +---------+---------+   +----------+----------+  |
|            |                        |             |
|            v                        v             |
|  +---------------------------------------------+  |
|  | Per-session state (Arc<Session>)            |  |
|  |   chromiumoxide::Page                       |  |
|  |   current_url, RefTable, background tasks   |  |
|  +-----------------------+---------------------+  |
|                          |                        |
+--------------------------|------------------------+
                           | CDP (WebSocket)
                           v
                  +-----------------+
                  | Chromium        |
                  |  + injected JS  |  (isolated world "acb_world")
                  |    helper       |
                  +-----------------+
                           |
                           v
                       Web pages
```

## Process layout

- **acb-daemon**: long-running. Owns Chromium and the policy. Crashes
  fatally; nothing keeps running in a broken state.
- **acb-cli**: short-lived. Talks to the daemon over its local HTTP API.
  Persists current session id in `${XDG_RUNTIME_DIR}/access-control-browser.cli.json`.
- **Chromium**: launched as a child process by chromiumoxide.

## Crates

- `acb-policy` — pure logic. Loads `config.yaml`, exposes `validate_url`,
  `decide_request`, `allowed_classes_for`. No I/O, no async.
- `acb-daemon` — axum HTTP layer + chromiumoxide CDP layer. Embeds the
  UI bundle via `rust-embed`.
- `acb-cli` — clap CLI; reqwest client of the daemon. Also exposes the
  `acb_cli::edit` / `acb_cli::sign` library (the `edit-config` /
  `sign-config` / `verify-config` policy-file signing, driven through
  `ssh-keygen -Y sign`).
- `crates/injected-js` — the fixed DOM helper, checked-in JS. SHA-256
  is computed at daemon startup and surfaced at `/config` for tampering
  detection.

## Key invariants

1. The agent never supplies a CSS selector or a JS expression to the
   daemon. Every action is identified by an `@eN` ref that the daemon
   itself allocated in the most-recent snapshot. Stale refs are 410.
2. The single `Runtime.evaluate`-like call in the daemon code base is the
   helper bootstrap; CI greps the source to enforce this.
3. The injected helper lives in a CDP isolated world; page scripts can't
   touch it.
4. Policy lookups during a Fetch decision use `arc_swap::ArcSwap`, so
   hot-reload is atomic from any in-flight request's point of view.
5. URL validation is one function (`validate_url`); CLI, UI, daemon
   handler, and request interceptor all call it.
