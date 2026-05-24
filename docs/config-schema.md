# config.yaml schema

Strict YAML with `deny_unknown_fields` at every level. Edit-and-save
reloads in place via the daemon's `notify` watcher (or a poll fallback on
hosts where inotify doesn't propagate, e.g. macOS bind mounts).

```yaml
server:
  bind: "127.0.0.1"                # 0.0.0.0 only with --insecure-bind
  port: 39100                      # port competition is fatal; do NOT renumber
  log_file: "./logs/access-control-browser.log"
  log_rotation: "daily"            # daily | hourly | never
  config_poll_ms: 0                # 0 = inotify, >0 = polling fallback

chromium:
  binary: null                     # null = auto-detect chromium / google-chrome
  user_data_dir: "./var/profile"
  viewport:
    width: 1280
    height: 800
  screencast:
    format: "jpeg"                 # jpeg | png
    quality: 70                    # 1..100
    max_fps: 8
  extra_args:                      # appended to Chromium command line
    - "--disable-features=WebRTC,WebTransport,SharedArrayBuffer"
    - "--disable-background-networking"

resource_policy:
  subresources_inherit_page: true  # allowed page's sub-resources auto-allowed
  always_block_schemes:            # never reached even via inheritance
    - "javascript"
    - "data"
    - "file"
    - "chrome"
    - "about"
    - "blob"
    - "ws"
    - "wss"
  bypass_service_worker: true      # Network.setBypassServiceWorker(true) per session

rules:
  # First match wins. Three rule kinds:

  - name: "github"
    match: { kind: "fqdn", host: "github.com", subdomains: false }
    allowed_classes: ["js-issue-row", "Box-row"]

  - name: "wikipedia-any-lang"
    match: { kind: "fqdn", host: "wikipedia.org", subdomains: true }
    allowed_classes: ["mw-parser-output", "vector-menu-content"]

  - name: "internal-docs"
    match:
      kind: "regex"
      pattern: "^https://docs\\.internal\\.example\\.com/(api|guide)(/.*)?$"
    allowed_classes: ["doc-content"]

  - name: "lan-grafana"
    match:
      kind: "ip_cidr"
      cidr: "10.0.0.0/8"
      ports: [3000]                # default: any port
      schemes: ["http", "https"]   # default: any http/https
    allowed_classes: ["dashboard-panel"]
```

## Validation done at load

- Unknown YAML keys reject the whole file (`deny_unknown_fields`).
- Duplicate rule names reject.
- `regex` is compiled with `size_limit` and `dfa_size_limit` ceilings.
- `cidr` must parse as `ipnet::IpNet` (v4 or v6).
- `ports` must be 1..65535; `schemes` limited to `http`/`https`.
- `server.port` must be non-zero.

## allowed_classes semantics (subtree-allowed)

An element is reachable by the agent iff:
- its `classList` contains one of the rule's `allowed_classes`, **or**
- an ancestor element (across open shadow roots) has such a class.

An empty `allowed_classes: []` means "the URL loads but nothing is
interactable/readable by the agent" — a useful mode for read-only
observation (the operator can still see the page in the UI).

## Path resolution

Relative paths in `server.log_file` and `chromium.user_data_dir`
**resolve against the config file's parent directory**, not the
daemon's CWD. This lets you place an entire daemon installation under
a single directory that's outside any LLM agent's writable area:

```
/etc/access-control-browser/
├── config.yaml          ← root-owned, read-only for the daemon user
├── logs/                ← log_file: "./logs/access-control-browser.log"
└── var/profile/         ← user_data_dir: "./var/profile"
```

Then start the daemon with `--config /etc/access-control-browser/config.yaml`
from any working directory — the paths above always land inside
`/etc/access-control-browser/`.

Absolute paths (`/var/log/...`, `/srv/...`) pass through unchanged. The
backwards-compatible default `--config ./config.yaml` still puts
`./logs/` and `./var/` next to the config file (which in that case is
also next to your CWD).

To make the "root-owned, read-only for the daemon user" layout above
actually enforceable against the agent, lock the file with
`acb-cli protect-config` and edit it through `acb-cli edit-config`
(validates before saving, then re-locks). See
[security-model.md §7](./security-model.md) and the "Protecting the
policy file" section of [usage.md](./usage.md).

## Etag

On load the file's raw bytes are SHA-256'd; the hash appears as `etag`
in `GET /config` so clients can detect a reload without diffing.
