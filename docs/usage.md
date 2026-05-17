# Usage guide (Linux / macOS / Windows)

Two practical paths exist today:

- **Docker (any OS)**: pull the prebuilt multi-arch image from
  `ghcr.io/fuba/access-control-browser:latest`. Works on Linux, macOS
  (Intel + Apple Silicon), and Windows under Docker Desktop. **This is
  the recommended path.**
- **Native build (Linux / macOS only in v0.1)**: `cargo build --release`
  produces standalone binaries. Native Windows `.exe` is not supported
  yet — see [Windows](#windows) for the reason and the workarounds.

Required files no matter the OS:

- `config.yaml` — the policy. Start from `config.example.yaml`.
- A writable directory for the rolling log file (`./logs/` by default).
- Access to a Chromium / Google Chrome binary. The Docker image bundles
  Chromium; native installs need it on `$PATH` or via
  `chromium.binary: "/path/to/chrome"` in `config.yaml`.

Once running, three things are useful to know:

- **UI**: `http://127.0.0.1:39100/?token=<token>`. The live viewport
  panel shows the page as Chromium renders it; click and type to drive
  the browser (Japanese IME works — `compositionupdate`/`end` events
  are forwarded as CDP `Input.imeSetComposition`/`Input.insertText`).
- **Token file** (the daemon writes it on startup):
  - Linux: `${XDG_RUNTIME_DIR}/access-control-browser.token` or
    `/tmp/access-control-browser.token`
  - macOS: `/tmp/access-control-browser.token`
  - Docker: inside the container at `/tmp/access-control-browser.token`
  - Windows (Docker): same — read it from the container with
    `docker exec acb cat /tmp/access-control-browser.token`
- **CLI**: `acb-cli` reads the token automatically from the path above.

---

## Linux

### Quick start with Docker

```bash
# 1. Get the example config and edit it.
curl -L -o config.yaml \
  https://raw.githubusercontent.com/fuba/access-control-browser/main/config.example.yaml
$EDITOR config.yaml

# 2. Run.
docker run -d --name acb \
  -p 39100:39100 \
  -v "$PWD/config.yaml:/app/config.yaml:ro" \
  -v "$PWD/logs:/app/logs" \
  ghcr.io/fuba/access-control-browser:latest

# 3. Get the token (printed inside the container).
TOKEN=$(docker exec acb cat /tmp/access-control-browser.token)
echo "open http://127.0.0.1:39100/?token=$TOKEN"

# 4. CLI from the host (talks to the container).
docker exec -i acb env ACB_BASE=http://127.0.0.1:39100 \
  acb-cli open https://github.com/
```

### Quick start with `docker compose`

```bash
git clone git@github.com:fuba/access-control-browser.git
cd access-control-browser
cp config.example.yaml config.yaml      # edit to taste
docker compose up -d
TOKEN=$(docker exec acb cat /tmp/access-control-browser.token)
xdg-open "http://127.0.0.1:39100/?token=$TOKEN"
```

### Native build (Linux)

```bash
# Prereqs
sudo apt-get install -y chromium       # or chromium-browser / google-chrome
rustup install stable
# Node is required only to build the UI bundle that the daemon embeds.
# Install Node 22+ from your distro or nvm.

git clone git@github.com:fuba/access-control-browser.git
cd access-control-browser
./scripts/build-ui.sh                  # produces ui/out/
cargo build --release --bin acb-daemon --bin acb-cli

# Run.
./target/release/acb-daemon --foreground --headless --config ./config.yaml
# In another shell:
./target/release/acb-cli status
./target/release/acb-cli open https://github.com/
```

The token lives at `${XDG_RUNTIME_DIR}/access-control-browser.token`
(systemd-managed user sessions) or `/tmp/access-control-browser.token`
(everything else). `acb-cli` picks the right one automatically.

---

## macOS

### Quick start with Docker Desktop

Docker Desktop on macOS runs Linux containers under a hypervisor, so the
**same Linux image works for both Intel and Apple Silicon Macs**. The
image is multi-arch; Docker will pull the right one.

```bash
# 1. Install Docker Desktop for Mac (Apple Silicon or Intel build).
# 2. Get the example config and edit it.
curl -L -o config.yaml \
  https://raw.githubusercontent.com/fuba/access-control-browser/main/config.example.yaml
open -e config.yaml

# 3. Run. macOS bind mounts go through gRPC FUSE, which does NOT deliver
#    inotify events — set config_poll_ms in config.yaml so the daemon
#    falls back to mtime polling for hot reload.
docker run -d --name acb \
  -p 39100:39100 \
  -v "$PWD/config.yaml:/app/config.yaml:ro" \
  -v "$PWD/logs:/app/logs" \
  ghcr.io/fuba/access-control-browser:latest

# 4. Open the UI.
TOKEN=$(docker exec acb cat /tmp/access-control-browser.token)
open "http://127.0.0.1:39100/?token=$TOKEN"
```

In `config.yaml` for macOS hosts:

```yaml
server:
  config_poll_ms: 1000     # inotify doesn't bubble through FUSE on macOS
```

### Native build (macOS)

```bash
# Prereqs
brew install rust node chromium        # or `brew install --cask google-chrome`
git clone git@github.com:fuba/access-control-browser.git
cd access-control-browser
./scripts/build-ui.sh
cargo build --release --bin acb-daemon --bin acb-cli

# Tell the daemon where Chromium lives (brew install path).
cat > config.yaml <<'YAML'
# ... copy from config.example.yaml ...
chromium:
  binary: "/opt/homebrew/bin/chromium"   # or "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
YAML

./target/release/acb-daemon --foreground --headless --config ./config.yaml
```

Token path on macOS is `/tmp/access-control-browser.token` (there is no
`XDG_RUNTIME_DIR` by default).

---

## Windows

**Native `acb-daemon.exe` / `acb-cli.exe` are not produced in v0.1.**
Three Unix-only code paths block a Windows-native build:

- `0600` mode bits on the token file (`std::os::unix::fs::PermissionsExt`)
- `$XDG_RUNTIME_DIR` fallback for token / CLI state file
- Chromium path auto-detection

These are tracked for a v0.3 fix. Until then, use one of the supported
paths below.

### Docker Desktop on Windows (recommended)

Identical UX to Linux. Docker Desktop's "WSL 2 backend" is the most
reliable mode.

```powershell
# In PowerShell, after installing Docker Desktop with WSL 2 backend.
Invoke-WebRequest `
  -Uri https://raw.githubusercontent.com/fuba/access-control-browser/main/config.example.yaml `
  -OutFile config.yaml
notepad config.yaml

docker run -d --name acb `
  -p 39100:39100 `
  -v "${PWD}/config.yaml:/app/config.yaml:ro" `
  -v "${PWD}/logs:/app/logs" `
  ghcr.io/fuba/access-control-browser:latest

$token = docker exec acb cat /tmp/access-control-browser.token
Start-Process "http://127.0.0.1:39100/?token=$token"
```

To use the CLI from PowerShell, shell into the container:

```powershell
docker exec -it acb acb-cli open https://github.com/
docker exec -it acb acb-cli snapshot
```

As on macOS, set `server.config_poll_ms: 1000` in `config.yaml` so hot
reload works through the WSL 2 FUSE mount.

### WSL 2 directly (no Docker)

Inside a WSL 2 distro (Ubuntu / Debian), follow the [Linux native
build](#native-build-linux) section verbatim. The daemon binds to the
WSL VM's loopback; Windows forwards `localhost:39100` to it
automatically.

```bash
# In a WSL 2 Ubuntu shell:
sudo apt update && sudo apt install -y chromium-browser build-essential pkg-config libssl-dev
# Install rust + node as on Linux, then:
./scripts/build-ui.sh
cargo build --release
./target/release/acb-daemon --foreground --headless --config ./config.yaml
```

From Windows (browser, PowerShell), `http://127.0.0.1:39100/...` works.

### Windows containers / Hyper-V isolation

Not supported. The Dockerfile is Debian-based; the runtime needs Linux
Chromium plus the GTK / Cairo / NSS stack. Use the Linux container path
above instead.

---

## Configuration reference

See [docs/config-schema.md](./config-schema.md) for the full YAML schema
including all match kinds (`regex` / `fqdn` / `ip_cidr`), the resource
policy switches, and validation rules.

## CLI reference

```
acb-cli validate <url>                 # offline check against ./config.yaml
acb-cli status                         # ping the daemon
acb-cli config                         # show redacted policy + helper sha256
acb-cli reload                         # nudge a manual policy reload
acb-cli open <url>                     # create / reuse a session and navigate
acb-cli snapshot                       # list accessible @eN refs
acb-cli click <@eN>                    # click an element
acb-cli fill <@eN> <text>              # fill a form field
acb-cli type <@eN> <text>              # type (append) into a field
acb-cli press <@eN> <key>              # dispatch a keydown/up
acb-cli hover <@eN>                    # mouse-hover
acb-cli select <@eN> <value>           # pick a <select> option
acb-cli check <@eN> <true|false>       # set a checkbox / radio
acb-cli find role|text <query>         # accessibility-restricted search
acb-cli close                          # close the current session
```

`--base http://host:port` and `--token-file <path>` overrides are
available on every command; `ACB_BASE`, `ACB_TOKEN`, and
`ACB_TOKEN_FILE` env vars work too.

## Troubleshooting

### "daemon not responding" from `acb-cli`

The CLI tries `http://127.0.0.1:39100` by default. If the daemon is in a
container on a different port, set `ACB_BASE=http://...`. If the token
file is at a non-default path (e.g. when running across host/container),
set `ACB_TOKEN_FILE=<path>` or `ACB_TOKEN=<raw token>`.

### "403 blocked" on every URL

`config.yaml` has no rule that matches the URL, or the scheme is in
`always_block_schemes`. Try `acb-cli validate <url>` to see why.

### "410 stale or unknown ref"

The DOM has been re-snapshotted since the ref was issued (every
`acb-cli snapshot` bumps the generation, invalidating prior refs). Take
a fresh snapshot and retry.

### macOS / Windows host edits to `config.yaml` don't reload

Set `server.config_poll_ms: 1000` in `config.yaml` so the daemon falls
back to mtime polling. Bind-mounted volumes on Docker Desktop don't
deliver inotify.

### Chromium fails to launch

In the Docker image, Chromium runs with `--no-sandbox`; the container is
the security boundary. Native macOS / Linux installs need Chromium or
Chrome on `$PATH`, or explicit `chromium.binary` in `config.yaml`.

### Token leaks via URL bar

`?token=` in the UI URL goes into browser history and the daemon's HTTP
access log. Treat the token as a session secret: regenerate by
restarting the daemon. Set `server.bind: "127.0.0.1"` (the default) so
the token only matters on the loopback.
