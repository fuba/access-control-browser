# Security model

Threat model and the concrete code paths that enforce each control.

## Threat actors

- **The LLM-driven agent** (untrusted). Operates the browser via the CLI or
  the UI's chat panel. Attempts to navigate to disallowed URLs, exfiltrate
  data via downloads, inject scripts via the location bar, or read elements
  the operator did not approve.
- **Web pages** (untrusted). May try to escape via popups, service workers,
  WebRTC data channels, or DNS rebinding.
- **Network attackers** on the localhost loopback (assumed low). The daemon
  binds to `127.0.0.1` by default; only `--insecure-bind` exposes it.

## Operator (trusted)

The operator writes `config.yaml`. Their judgment about which CSS classes
make an element "accessible" is taken as authoritative — if the operator
allows `mw-parser-output` on Wikipedia, every descendant in that subtree is
readable and clickable. There is no opinion built into the daemon about
whether that's appropriate; document it as part of the operator's policy
review.

## Controls

### 1. URL allowlist (req §1)

- `acb_policy::url_validator::validate_url` is the single decision point.
- Decision order: malformed parse → scheme deny list → http/https only →
  rule walk (regex / FQDN exact / FQDN-subdomain / IP CIDR with optional
  port and scheme filters) → first match wins.
- Callers: `acb-cli validate`, `POST /sessions/:id/open`, the daemon's CDP
  `Fetch.requestPaused` handler (top-level branch), and the UI's
  `lib/url-validator.ts` for live indicator (advisory; server is
  authoritative).
- **userinfo URLs are rejected** as a `MalformedUrl` reason, because the
  host after the `@` is the real destination and the form invites
  allowlist-confusion.

### 2. Sub-resource policy (req §2)

- `resource_policy.subresources_inherit_page` (default `true`) lets an
  allowed top-level page load its CDN resources regardless of origin —
  required for any real site to function.
- The page's current top-level URL is tracked from
  `Page.frameNavigated` events into `Session::current_url`.
- `acb_policy::request_policy::decide_request` classifies each request
  (TopLevelDocument / SubFrameDocument / Subresource), applies inheritance,
  and is the function the interceptor calls.
- **Defense in depth**: the scheme deny list is rechecked at every request
  level, even when inheritance is on. `javascript:`, `data:`, `file:`,
  `chrome:`, `about:`, `blob:`, `ws:`, `wss:` never pass.

### 3. Element-class subtree restriction (req §3)

- The injected DOM helper at `crates/injected-js/dist/snapshot-helper.js`
  runs in an isolated world (`Page.addScriptToEvaluateOnNewDocument` with
  `worldName="acb_world"`). Page scripts cannot reach `__acb` because they
  live in a different JavaScript context.
- The helper's `collect(allowedClasses)` walks the DOM (including open
  shadow roots via composed parent chain) and returns metadata only for
  elements whose own classList contains an allowed class OR whose ancestor
  does.
- Closed shadow roots are out of scope: inaccessible by design.
- Action handlers (`click`, `fill`, `type`, etc.) accept only `@eN` refs
  allocated by the most recent snapshot. A stale ref (different
  generation) returns `410 Gone`; an unknown ref returns `410` too.

### 4. No agent-side code injection (req §5)

| Vector | Control |
|---|---|
| `Runtime.evaluate(expression=...)` with agent text | No such code path. The only `Runtime.evaluate`-like call is the helper bootstrap, with a fixed string. CI grep gate forbids `Runtime\.evaluate\|expression\s*:` outside `crates/daemon/src/browser/injected.rs`. |
| `eval` tool exposed to agent | None. The API surface is verb-based: click, fill, type, press, hover, select, check, find. |
| `javascript:` / `data:` / etc. via location bar | `validate_url` rejects them before any browser call. |
| Raw selector finder (escape via CSS selector) | None. `find` calls the helper's `findRole` / `findText`, which operate only over the policy-filtered accessible set. |
| Tampered injected helper | The helper's SHA-256 is computed at daemon startup and surfaced at `/config`; tampering is detectable by comparing the hash to the checked-in source. |
| Refs forged by the agent | Refs are server-allocated and validated against `^@e[0-9]+$` plus the live ref table. Monotonic numbering across snapshots, generation-checked. |
| Popups / new tabs to escape the allowlist | `Target.targetCreated` handler auto-closes any popup whose URL fails `validate_url`. |
| Downloads as exfiltration | `Browser.setDownloadBehavior(Deny)` per session. |
| Service workers shadowing the network | `Network.setBypassServiceWorker(true)` per session; `Storage.clearDataForOrigin(serviceworkers)` is recommended at session start (TODO). |
| WebRTC / WebTransport data channels | Chromium launched with `--disable-features=WebRTC,WebTransport,SharedArrayBuffer`. |
| Unknown JSON fields slipping into a request body | Every request struct uses `#[serde(deny_unknown_fields)]`. |
| Oversized text | 8 KiB cap on every `text` field handed to the helper. |

### 5. Localhost-only auth (req §6)

- Bearer token: 32 random bytes, base64-url, written to
  `${XDG_RUNTIME_DIR:-/tmp}/access-control-browser.token` with mode 0600
  at daemon start. Rotates on every start.
- Constant-time comparison in the middleware.
- `EventSource` (SSE) clients fall back to `?token=` query parameter
  because browsers can't add custom headers to `EventSource`. Documented
  trade-off — operators with strict logging concerns should not enable
  `--insecure-bind`.

### 6. Hot reload (req §1 + operator UX)

- `notify-debouncer` watcher (with poll fallback for bind mounts on
  hosts where inotify doesn't propagate).
- `ArcSwap<CompiledPolicy>` atomic swap ensures in-flight Fetch decisions
  use a coherent policy.
- Malformed reload keeps the previous policy and emits
  `policy.reload_failed` on the SSE feed.

## Known limitations

- **DNS rebinding**: if a DNS server controlled by an attacker resolves an
  allowed FQDN to an internal IP, our FQDN rule passes but the actual
  destination is internal. Mitigate by combining FQDN rules with IP CIDR
  rules where possible, or by running Chromium with a fixed DoH resolver
  (`--dns-over-https-templates=...`).
- **Closed shadow DOM**: unreachable. Treated as a safe-default in v0.1.
- **Live viewport (screencast)**: not implemented in v0.1; UI shows a
  placeholder. Planned for v0.2.
- **Pixel-level redaction**: a screenshot or screencast can show data
  outside the allowed class subtree. The class restriction is for
  programmatic access, not visual confidentiality.
