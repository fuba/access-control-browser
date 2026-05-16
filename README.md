# access-control-browser

An access-controlled browser for LLM coding agents. Operated via a CLI **and** a web GUI; both drive the same daemon.

## What it does

- Opens only URLs that match rules in `./config.yaml` (regex / FQDN / IP CIDR).
- Allowed pages can load their own sub-resources (JS, CSS, images) so real sites work.
- Per URL pattern, `config.yaml` lists CSS classes whose elements (and their subtree) the agent is allowed to read or interact with — nothing else is reachable.
- The LLM agent cannot inject scripts. No `eval`-equivalent API; the location bar rejects `javascript:` and any non-allowlisted URL; element refs are server-allocated.
- UI inspired by [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) (live viewport, snapshot refs, chat panel) with the eval / console / extensions / raw-selector surfaces removed.
- Ships as a multi-arch Docker image (`linux/amd64`, `linux/arm64`).

## Status

Pre-v0.1. See `docs/` and the implementation plan for the roadmap.

## License

MIT. See `LICENSE`.
