# access-control-browser

An access-controlled browser for LLM coding agents. Operated via a CLI **and** a web GUI; both drive the same daemon.

## What it does

- Opens only URLs that match rules in `./config.yaml` (regex / FQDN / IP CIDR).
- Allowed pages can load their own sub-resources (JS, CSS, images) so real sites work.
- Per URL pattern, `config.yaml` lists CSS classes whose elements (and their subtree) the agent is allowed to read or interact with — nothing else is reachable.
- The LLM agent cannot inject scripts. No `eval`-equivalent API; the location bar rejects `javascript:` and any non-allowlisted URL; element refs are server-allocated.
- UI inspired by [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) (live viewport, snapshot refs, chat panel) with the eval / console / extensions / raw-selector surfaces removed.
- Ships as a multi-arch Docker image (`linux/amd64`, `linux/arm64`).

## Get started

Pick the path for your OS:

- **Docker (any OS — recommended)**: `docker pull ghcr.io/fuba/access-control-browser:latest`. The image is multi-arch (`linux/amd64`, `linux/arm64`).
- **Linux native**, **macOS native** (Apple Silicon or Intel), **Windows via Docker Desktop / WSL 2** — see [docs/usage.md](./docs/usage.md) for step-by-step instructions, the CLI reference, and troubleshooting.

Quick taste once it's running:

```bash
TOKEN=$(docker exec acb cat /tmp/access-control-browser.token)
open "http://127.0.0.1:39100/?token=$TOKEN"   # the UI
docker exec acb acb-cli open https://github.com/   # from the CLI
```

## Docs

- [docs/usage.md](./docs/usage.md) — Linux / macOS / Windows install and run
- [docs/config-schema.md](./docs/config-schema.md) — full `config.yaml` schema
- [docs/api.md](./docs/api.md) — daemon HTTP/SSE endpoints
- [docs/architecture.md](./docs/architecture.md) — process layout and invariants
- [docs/security-model.md](./docs/security-model.md) — threat model and per-control mapping

## Status

v0.2.x. The daemon, CLI, UI, and policy are working end-to-end. Live viewport (CDP screencast) and Windows-native `.exe` are deferred to v0.3.

## License

MIT. See `LICENSE`.
