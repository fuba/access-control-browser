# Daemon HTTP API

Localhost-only by default (`127.0.0.1:39100`). Bearer auth on every
endpoint except `/healthz`. SSE accepts `?token=` as a fallback because
`EventSource` cannot send custom headers.

```
GET    /healthz                          unauth, returns x-acb:1 header
GET    /config                           redacted policy summary
GET    /sessions                         list sessions [{id,url,title,created_at}]
POST   /sessions                         create a new browser session
DELETE /sessions/:id                     close session
POST   /sessions/:id/open  {url}         navigate (re-validates URL)
POST   /sessions/:id/back                history back (pre-validates entry URL)
POST   /sessions/:id/forward             history forward (pre-validates entry URL)
POST   /sessions/:id/reload              reload current page
POST   /sessions/:id/snapshot            policy-filtered DOM with @eN refs
POST   /sessions/:id/click   {ref}
POST   /sessions/:id/fill    {ref,text}
POST   /sessions/:id/type    {ref,text}
POST   /sessions/:id/press   {ref,key}
POST   /sessions/:id/hover   {ref}
POST   /sessions/:id/select  {ref,value}
POST   /sessions/:id/check   {ref,checked}
POST   /sessions/:id/find    {kind: role|text, query}
GET    /sessions/:id/viewport            WS: live viewport + operator input
GET    /events                           SSE activity feed (auth via ?token)
POST   /admin/reload                     manual policy reload
GET    /                                 embedded Next.js UI
GET    /*                                static UI assets
```

`back` / `forward` pre-validate the destination history entry's URL with
the allowlist and return **403** if it's no longer allowed — the Fetch
interceptor alone can't gate history navigation because Chromium may
restore a cached document before the network request fires.

The SSE `/events` feed includes a `session_url` event
(`{session, url, title}`) emitted on navigation / title change, which the
UI uses to keep the location bar and tab labels live.

## `/sessions/:id/viewport` WS

Bidirectional. Server → client is binary JPEG frames (lazy-started at
~6 fps via `Page.captureScreenshot`; cached most-recent frame is sent on
connect so a fresh subscriber sees something immediately). Client →
server is JSON with `kind`:

| `kind` | fields |
|---|---|
| `mouse` | `type` (`down`/`up`/`move`/`wheel`), `x`, `y`, optional `button` (`left`/`middle`/`right`), `click_count`, `modifiers` |
| `key` | `type` (`down`/`up`), `key`, `code`, optional `text`, `modifiers` |
| `composition_start` | — |
| `composition_update` | `text`, optional `selection_start`/`selection_end` |
| `composition_end` | `text` (committed) |

IME usage is the standard browser pattern: the UI listens to the page's
own `compositionstart`/`update`/`end` events on an invisible textarea
overlay and forwards them. The daemon translates each to
`Input.imeSetComposition` / `Input.insertText`. Test:
`crates/daemon/tests/viewport_input.rs::japanese_ime_inserts_text`.

## Status codes

- `200/204` success
- `400` invalid JSON or unknown field
- `401` missing/wrong token
- `403` URL blocked by policy
- `404` no such session
- `410` stale or unknown ref
- `413` text payload too large (>8 KiB)
- `500` internal error
- `502` browser navigation failed

## Example: open + snapshot + click

```
TOKEN=$(cat ${XDG_RUNTIME_DIR:-/tmp}/access-control-browser.token)
BASE=http://127.0.0.1:39100

# Create session
SID=$(curl -s -H "Authorization: Bearer $TOKEN" -X POST $BASE/sessions | jq -r .id)

# Open
curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -X POST $BASE/sessions/$SID/open \
  -d '{"url":"https://github.com/"}'

# Snapshot
curl -s -H "Authorization: Bearer $TOKEN" -X POST $BASE/sessions/$SID/snapshot | jq

# Click @e3
curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -X POST $BASE/sessions/$SID/click \
  -d '{"ref":"@e3"}'
```

Or via `acb-cli`, which wraps all of the above and persists the session id
in `${XDG_RUNTIME_DIR}/access-control-browser.cli.json`:

```
acb-cli open https://github.com/
acb-cli snapshot
acb-cli click @e3
```
