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
- **The live viewport path (`/sessions/:id/viewport` WS) is exempt from
  the class restriction**: the human operator driving the UI is trusted
  and the class system exists to constrain the LLM agent. URL allowlist
  rules still bind every navigation, including those triggered by
  operator clicks (they go through the Fetch interceptor). The
  unrestricted input surface is documented at v0.2.

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
| Rewriting `config.yaml` to self-authorize | No policy-write HTTP endpoint exists (`/config` is read-only; `/admin/reload` re-reads the file and takes no body). Filesystem tampering is addressed by §7: the daemon only loads a policy signed by the operator (`--verify-key`). |

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
  hosts where inotify doesn't propagate). Both `config.yaml` and
  `config.yaml.sig` are watched.
- `ArcSwap<CompiledPolicy>` atomic swap ensures in-flight Fetch decisions
  use a coherent policy.
- Every load — startup, watcher, `/admin/reload` — goes through
  `acb_daemon::policy_file::load`, the single place that verifies the
  signature (when enabled) and checks the revision, so no path bypasses it.
- Malformed reload, failed signature check or revision rollback keeps the
  previous policy and emits `policy.reload_failed` on the SSE feed.

### 7. Policy file integrity (operator trust boundary)

The policy is authored by the trusted operator; the agent must not be able to
edit it. Two layers:

- **No write path through the daemon.** The HTTP API is verb-based and has no
  config-mutation endpoint. `GET /config` is read-only and redacted;
  `POST /admin/reload` re-reads the on-disk path and accepts no body. So the
  agent cannot change the policy *via the browser it drives*.
- **Signed policy (`acb-daemon --verify-key` / `acb-cli sign-config`).**
  Because a coding agent usually shares a filesystem — and a user account —
  with `config.yaml`, permissions cannot separate the two, and a root-owned
  file would make every edit a sudo prompt. Instead the daemon is given a
  file of trusted OpenSSH public keys and loads `config.yaml` only if
  `config.yaml.sig` is a valid **sshsig** (`ssh-keygen -Y sign`, namespace
  `acb-policy`) over the exact bytes by one of those keys. Verification is
  `acb_policy::sig` (pure, `ssh-key` crate: ed25519 and ECDSA P-256, which
  covers software keys, FIDO2 `-sk` keys and Secure-Enclave/TPM keys).
  Two signers produce that envelope: `ssh-keygen -Y sign` (any key OpenSSH
  can use) and, on macOS, `acb_cli::se` — a P-256 key generated in the
  Secure Enclave with `kSecAccessControlUserPresence`, so
  `SecKeyCreateSignature` itself triggers Touch ID / password and the
  private key is non-exportable. The DER signature is re-encoded as
  `ecdsa-sha2-nistp256` sshsig and self-verified before it is written.
  - **Property guaranteed**: the daemon the operator started never loads a
    policy the operator did not sign. Integrity/authenticity is what is
    needed here, not confidentiality (`GET /config` and `acb-cli validate`
    read the policy anyway), which is why this is a signature and not
    encryption: the daemon needs no secret at all, so it starts unattended,
    hot-reloads unattended, and runs in a container, while the *signing*
    key can be gated by a human-presence check (FIDO2 touch, Touch ID via a
    Secure-Enclave ssh-agent, `ssh-add -c` confirmation, or a passphrase).
  - **Rollback guard**: the signed document carries a top-level
    `revision:`; a reload is refused if it is lower than the revision in
    effect. Equal is accepted (re-save). `edit-config` bumps it on every
    signed save. The guard is in-memory: it protects a running daemon.
  - **No parent-directory precondition**: an agent that swaps the directory
    entry or rewrites the file only produces something that fails
    verification, and the previous policy stays in effect. The file may
    live anywhere, including inside the agent's working tree.
  - **Preconditions**: the verify-key file and the daemon's launch
    definition must not be agent-writable, and the agent must not be able
    to restart the operator's daemon with other arguments (an agent could
    always run its own `acb-daemon --config /tmp/x.yaml`; that daemon is
    not the one the operator's tools point at). The signing key must not be
    usable non-interactively by the agent: an unencrypted key file
    protects nothing; a weak passphrase can be brute-forced offline; the
    agent's terminal must not hold accessibility rights that let it click
    through the agent/askpass prompts (a Touch ID prompt cannot be
    scripted, but the password fallback dialog is a system dialog like any
    other). The Secure Enclave path additionally assumes the `acb-cli`
    binary is code-signed and that the machine has an enclave; it is
    type-checked for macOS in CI but the enclave interaction itself needs
    real hardware to exercise.
  - **Write ordering**: `edit-config` and `sign-config` sign a staged copy
    in a private temp dir (also avoiding `ssh-keygen`'s overwrite prompt),
    install the signature first and the policy second (both via temp file +
    rename), so the watcher's final load always sees a consistent pair; a
    cancelled signature leaves both files untouched. A policy that already
    has a `.sig` cannot be saved unsigned by `edit-config` (the edit is
    parked, not installed).
  - **No shell in the editor/signer paths**: `$EDITOR` is split into argv
    and executed directly (no `sh -c`), and `ssh-keygen` is invoked with an
    argv, so spaces or metacharacters in paths cannot inject.
  - **Detection**: `policy.reloaded` events carry the signer's SHA-256
    fingerprint and the revision; `GET /config` exposes
    `signature_required` and `revision`.
  - **Defense in depth**: `compose.yml` bind-mounts the file `:ro`, so a
    compromised in-container daemon also cannot rewrite it.

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
