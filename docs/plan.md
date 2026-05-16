# Access-Controlled Browser for LLM Coding Agents — Implementation Plan

## Context

LLM coding agent が安全にブラウザを利用できるようにするための「アクセス制御ブラウザ」を新規作成する。`/home/ec/access-control-browser/` は現在空。

要件 (ユーザー入力の要約):
1. `./config.yaml` の URL ルール (regex / FQDN / IP CIDR) に**一致するサイトのみ**開ける。
2. 一致した URL のページが include する JS/CSS/画像/XHR 等のサブリソースは**そのまま読み込める**ようにする (サイトを実際に使えること)。
3. config.yaml に **URL パターンごとの「触ってよい HTML 要素の class」** を列挙でき、当該 class を持つ要素**および子孫サブツリー**だけが LLM から読み書きできる。
4. UI は https://github.com/vercel-labs/agent-browser と同様 (live viewport, chat, snapshot refs)。要件に反する機能 (eval/console/extensions injection/raw selector) は削除。
5. **LLM 由来コードのページ注入を許さない**: `eval` 系 API なし、locaction bar の `javascript:` 等を拒否、ref はサーバ採番のみ。
6. GUI と CLI 両方から同じ daemon を操作可能。
7. **Docker (linux/amd64 と linux/arm64 マルチアーチ)** で動作。

ユーザーが選んだスタック: **Rust + CDP**。UI は Next.js を静的エクスポートし daemon バイナリに `rust-embed` で同梱。Element class は **subtree-allowed** モード。

License: MIT (ユーザーの要件設計の貢献あり)。コードコメントは英語、対話は日本語 (CLAUDE.md 準拠)。テストファースト、モック禁止 (実 Chromium 利用)、`compose.yml` (no `version:` key)、daemon は背景常駐かつログファイル出力かつホットリロード、port 競合時にポートを変えない。

---

## Approach

### A. プロジェクト構成

Cargo workspace:

```
/home/ec/access-control-browser/
├── Cargo.toml                     # workspace
├── Dockerfile                     # multi-stage / multi-arch
├── compose.yml                    # no `version:` key
├── config.example.yaml
├── crates/
│   ├── policy/                    # 純粋ロジック (no I/O, no async) — ここから先にテスト
│   ├── daemon/                    # axum + chromiumoxide + 常駐プロセス
│   ├── cli/                       # acb-cli (clap + reqwest)
│   └── injected-js/               # 固定の DOM helper (TS→JS, ハッシュ固定)
├── ui/                            # Next.js 15 static export
└── docs/                          # architecture / security-model / config-schema / api
```

### B. 依存クレート (主要)

- async: `tokio` 1.40+
- CDP: **`chromiumoxide` 0.7** (async, tokio-native, `Fetch` / `Target` / `Page.addScriptToEvaluateOnNewDocument` を直接扱える)。代替の `headless_chrome` は同期、自前 tungstenite はコード量が爆発するため不採用。
- HTTP/WS/SSE: `axum` 0.7 + `tower-http`
- YAML: `serde_yaml` 0.9
- Regex: `regex` 1.10 (`size_limit` / `dfa_size_limit` を厳しく)
- CIDR: `ipnet` 2.9
- URL: `url` 2.5
- Watcher: `notify` 6.1 + `notify-debouncer-full` (macOS host 用にポーリングフォールバック)
- Atomic policy swap: `arc-swap` 1.7
- Auth: `rand` 0.8 + base64
- Log: `tracing` + `tracing-subscriber` + `tracing-appender` (daily rolling)
- UI 同梱: `rust-embed` 8
- 失敗ハンドリング: `thiserror` (lib), `anyhow` (bins)
- テスト: `tokio::test`, `assert_cmd`, `tempfile`, `pretty_assertions`, `reqwest`, `httptest` (実 HTTP fixture)
- Multi-arch ビルド: `cargo-zigbuild` 0.19

### C. config.yaml スキーマ

```yaml
server:
  bind: "127.0.0.1"              # 0.0.0.0 は --insecure-bind が必要
  port: 39100                    # 競合時は変更せず exit (CLAUDE.md 準拠)
  log_file: "./logs/access-control-browser.log"
  log_rotation: "daily"
  config_poll_ms: 0              # 0 = inotify, >0 = polling fallback (macOS host 用)

chromium:
  binary: null                   # null=auto-detect
  user_data_dir: "./var/profile"
  viewport: { width: 1280, height: 800 }
  screencast: { format: "jpeg", quality: 70, max_fps: 8 }
  # Hardened launch flags. 詳細は browser/launch.rs 参照
  extra_args:
    - "--disable-features=WebRTC,WebTransport,SharedArrayBuffer"
    - "--disable-background-networking"

resource_policy:
  subresources_inherit_page: true    # ページが許可なら CDN 等のサブリソースも許可
  always_block_schemes: ["javascript","data","file","chrome","about","blob","ws","wss"]
  bypass_service_worker: true        # Fetch interceptor を SW にバイパスされないため

rules:
  - name: "github"
    match: { kind: "fqdn", host: "github.com", subdomains: false }
    allowed_classes: ["js-issue-row","Box-row"]

  - name: "wikipedia-any-lang"
    match: { kind: "fqdn", host: "wikipedia.org", subdomains: true }
    allowed_classes: ["mw-parser-output","vector-menu-content"]

  - name: "internal-docs"
    match: { kind: "regex", pattern: "^https://docs\\.internal\\.example\\.com/(api|guide)(/.*)?$" }
    allowed_classes: ["doc-content"]

  - name: "lan-grafana"
    match: { kind: "ip_cidr", cidr: "10.0.0.0/8", ports: [3000], schemes: ["http","https"] }
    allowed_classes: ["dashboard-panel"]
```

Loader (`policy::load`):
- `#[serde(deny_unknown_fields)]` 全構造体
- 重複 rule 名禁止、regex サイズ制限、CIDR / ports 検証
- ロード時に SHA-256 計算し `etag` として `GET /config` で公開
- 改行/トレーリング以外で内容差分があればホットリロード発火

### D. URL アクセス制御の経路

すべて単一関数 **`policy::url_validator::validate_url(url, &cfg) -> Result<UrlClass, BlockReason>`** に集約。CLI / UI (TS port) / daemon の interceptor / `POST /open` ハンドラがすべてこれを通る。

判定順:
1. `url::Url::parse` 失敗 → `MalformedUrl`
2. scheme が `always_block_schemes` または `http|https` 以外 → `SchemeBlocked`
3. rules を順に評価し最初の一致 → `Allowed{rule_name}`
4. どれにも一致しなければ `NoMatchingRule`

CDP 側の運用 (`daemon::browser::interceptor`):
- セッション作成時に **`Fetch.enable`** (パターン全件、stage=Request)。`Network.setRequestInterception` は deprecated なので使わない。
- `Network.setBypassServiceWorker(true)` を発行し、SW が Fetch インターセプタを迂回しないようにする。セッション開始時に `Storage.clearDataForOrigin(serviceworkers)` で既存 SW を一掃。
- `Page.setDownloadBehavior(Deny)` でダウンロードを禁止 (ファイル経由の流出を遮断)。
- `Target.targetCreated` を購読し、ポップアップ/新タブの URL が `validate_url` で `Allowed` でなければ `Target.closeTarget`。
- WebSocket (`ws:/wss:`) は `always_block_schemes` で塞ぎつつ、CDP `Network.webSocket*` イベントを監視。WebRTC / WebTransport は Chromium 起動フラグで無効化。
- `Page.frameNavigated` で各 frame の URL を `Mutex<HashMap<FrameId, Url>>` に追跡し、サブリソース判定で参照。
- `Fetch.requestPaused` 受信:
  - **Top-level navigation** (`resourceType==Document && parentFrameId==None`): `validate_url`。不可なら `Fetch.failRequest(BlockedByClient)` + 活動フィードに通知。
  - **Subframe document**: 親フレームの URL の判定を継承 (subresources_inherit_page=true なら通す)。
  - **その他のサブリソース**: 同上。`subresources_inherit_page=false` のときはサブリソース自身の URL も `validate_url` する。
- ポリシーは `arc_swap::ArcSwap<PolicyConfig>` で保持。ホットリロード時にも in-flight イベントが整合性を保つ。

注記 (既知の限界、docs に明記):
- **DNS rebinding**: 攻撃者が制御する DNS が許可 FQDN を内部 IP に解決するケース。v1 では Chromium に DoH 固定 (`--dns-over-https-templates`) を推奨し、IP CIDR ルールと併用しなければ完全には防げないと明記。
- **closed Shadow DOM**: v1 では open shadow root のみ走査対象。closed の場合は要素アクセス不能 (= 安全側に倒す)。
- **WebRTC/WebTransport**: 起動フラグで無効化済み。

### E. 要素レベル制御 (subtree-allowed)

**Isolated world 方式** で page JS から helper を完全分離する (Plan agent の素案では `window.__acb` だったが、ページ側 JS に上書きされる可能性があるため変更)。

`crates/injected-js/src/snapshot-helper.ts` → `dist/snapshot-helper.js` (チェックイン、SHA-256 を `daemon/build.rs` で `include_bytes!` 時にハッシュ計算 → 起動時に再計算し一致しなければ起動拒否)。

daemon 側のロード手順:
1. ページ生成時に `Page.createIsolatedWorld(frameId, worldName="acb")` を呼び、独立 context id を得る。
2. その context に対し `Runtime.evaluate(expression=helperSource, contextId=...)` で固定ヘルパを評価。ヘルパは `globalThis.__acb` をその world 内のみに作成し `Object.freeze`。
3. **`Page.addScriptToEvaluateOnNewDocument(source, worldName="acb")`** で全 frame / 全 navigation に同じ world で再注入。
4. ヘルパ内で `Element.prototype.attachShadow` を early patch して open shadow root を `WeakMap` 経由で追跡 (composed traversal を可能にする)。closed shadow root は v1 では非対応 (= 不可触)。

ヘルパ API (daemon の `Runtime.callFunctionOn` でのみ呼ばれる):
- `collect(allowedClasses: string[]) -> AccessibleNode[]`
  - DOM を walk し、ある要素の `classList` が `allowedClasses` のどれかを含むか、祖先がそうなら accessible とマーク。
  - accessible な要素のみを返し、各々に `acbId` (per-document 連番) を採番。
  - 戻り値: `{ acbId, tag, role, name, text(≤200chars), href, bounding_rect, parent_acbId }`
- `resolve(acbId) -> JsHandle` (page side では `Element`、daemon は `objectId` を取得)
- `findRole(role) / findText(text)`: accessible セット内のみで検索
- これ以外の eval-like surface なし。

daemon 側 ref テーブル (`daemon::snapshot::ref_table`):
- per-session で `Generation` をインクリメント、`@e1, @e2, …` を採番。
- アクション (click/fill/type/press/hover/select/check) は `{ref}` のみを受け、現世代に無いなら **410 Gone**。
- 解決経路: ref → acbId → `Runtime.callFunctionOn(__acb.resolve, [acbId])` で `objectId` 取得 → `DOM.scrollIntoViewIfNeeded` → `DOM.getBoxModel` → `Input.dispatchMouseEvent` / `Input.insertText` などの**型付き CDP 呼び出しのみ**。文字列の式は一切渡さない。

iframe の扱い:
- iframe document が許可ページの場合、その内側にも同じ isolated world / helper を仕込む。
- `allowed_classes` は**トップレベル URL のルール**から取る (運用者がそのページを許可した責任で iframe 内も同等扱い)。docs に明記。

### F. CLI ↔ daemon API

- 既定 `127.0.0.1:39100`、Bearer token。
- Token: 32 byte random、base64-url、`${XDG_RUNTIME_DIR:-/tmp}/access-control-browser.token` に `0600` で書き出し、起動毎にローテート。親ディレクトリの sticky/書き込み権限を起動前検査。
- Docker 利用時は volume 経由でホストの CLI と共有可能、または `docker exec acb acb-cli ...` で内側実行。
- `#[serde(deny_unknown_fields)]` を全リクエスト/レスポンスに適用。

主要エンドポイント:
```
GET    /healthz                  # unauth, x-acb: 1 ヘッダで自プロセス判別
GET    /config                   # rule 名一覧 + helper sha256 + etag (read-only)
POST   /sessions                 # 作成
DELETE /sessions/:id
POST   /sessions/:id/open        { url } -> { url, rule_name }
POST   /sessions/:id/snapshot    -> { generation, refs:[{ref,tag,role,name,text,href,rect}] }
POST   /sessions/:id/click       { ref }
POST   /sessions/:id/fill        { ref, text }
POST   /sessions/:id/type        { ref, text }
POST   /sessions/:id/press       { ref, key }
POST   /sessions/:id/hover       { ref }
POST   /sessions/:id/select      { ref, value }
POST   /sessions/:id/check       { ref, checked }
POST   /sessions/:id/find        { kind: "role"|"text"|"label", query }   # accessible set 内のみ
POST   /sessions/:id/wait        { until, timeout_ms }
GET    /sessions/:id/screencast  (WS)         # 既存セッションに何度でも接続可
GET    /events                   (SSE)        # nav/block/click/policy.reloaded
GET    /*                        # UI (rust-embed)
POST   /admin/reload                          # notify と同じパスを叩く
```

**シングルインスタンス保証**: 起動時にポートを bind 失敗 → 既存に `GET /healthz` → `x-acb: 1` なら "already running" で 0 終了、それ以外は 1 で exit (ポート変更しない)。

**バックグラウンド化 + ログ**: `--foreground` で前面、デフォルトは double-fork。`tracing` を file appender に出力し、`--foreground` 時は stderr にもミラー。CLI 経由で `acb-cli logs -f` でフォロー可。

### G. UI (Next.js 静的エクスポート)

`next.config.mjs: { output: 'export' }`。daemon が `rust-embed` で `ui/out/` を埋め込み、`/` 配下で配信。CSP ヘッダで自前以外のソースを禁止。

コンポーネント:
- `Viewport`: WS で screencast JPEG をバイナリ受信、`<img>` に `URL.createObjectURL`。最新 snapshot の `@eN` 矩形を SVG オーバーレイ。
- `LocationBar`: `lib/url-validator.ts` (TS port of policy::url_validator、`GET /config` の rule で動作) でキー入力毎に色判定。submit で `POST /open`。
- `SnapshotPanel`: `@eN role text` の表、各行に Click/Fill ボタン。
- `ChatPanel`: `@ai-sdk/react`。tool 定義は daemon API と 1:1。Tool 実行は daemon へ HTTP 直叩き (静的エクスポートなので edge route なし)。**eval ツールは存在しない**。
- `ActivityFeed`: `EventSource('/events')`。block/allow/reload を表示。

vercel-labs/agent-browser から **削除**: Console, Eval, Extensions, Storage 書き込み, Network 改ざん, raw selector finder。

UI bundle のセキュリティ:
- CSP: `default-src 'self'; img-src 'self' blob:; connect-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'`
- `X-Frame-Options: DENY`、`Referrer-Policy: no-referrer`、`Permissions-Policy` で全許可機能を絞る
- Service worker なし

### H. アンチインジェクション保証 (テストでカバー)

- `validate_url` で scheme deny / 非許可 URL を拒否
- daemon に `Runtime.evaluate(expression=...)` をエージェント入力で渡すコードパスを**作らない**。CI で `grep -RE "Runtime\\.evaluate|expression\\s*:" crates/daemon/src` し、許可リスト (helper bootstrap の 1 箇所) 以外を検出したら CI 失敗。
- 全 ref はサーバ採番、`^@e[0-9]+$` を検証
- helper script は build 時ハッシュを `include_bytes!` 経由で固定、起動時に再ハッシュ照合
- raw selector エンドポイントなし、`find` は accessible set 内のみ
- ポップアップ自動クローズ、ダウンロード拒否、SW バイパス
- UI 側にも CSP
- リクエスト boy はすべて `deny_unknown_fields`、text 長は 8 KiB 上限など
- WebRTC/WebTransport は Chromium フラグで無効化

### I. Docker (multi-arch)

- ビルダー: `rust:1.83-slim-bookworm` + `cargo-zigbuild` (QEMU 不要で amd64/arm64 をワンビルダで)
- ランタイム: `debian:bookworm-slim` + apt の `chromium` (amd64/arm64 両方 OK) + `tini`
- UI ビルドは `node:22-bookworm-slim` ステージで実施、`ui/out/` を Rust ステージへ
- injected-js も同様に TS→JS をビルドし checksum を Rust 側でロック
- ランタイムでは **non-root user** (uid 1000)
- Chromium sandbox: 既定で `--no-sandbox` + Docker user namespace を使用。`SYS_ADMIN` 付与は `compose.yml` のデフォルトには含めない (権限最小化)。
- Multi-arch:
  ```
  docker buildx create --use --name acb-builder
  docker buildx build --platform linux/amd64,linux/arm64 \
    -t ghcr.io/<owner>/access-control-browser:0.1.0 --push .
  ```
- `compose.yml` (no `version:`):
  ```yaml
  services:
    acb:
      build: { context: ., dockerfile: Dockerfile }
      image: access-control-browser:dev
      init: true
      ports: ["39100:39100"]
      volumes:
        - ./config.yaml:/app/config.yaml:ro
        - ./logs:/app/logs
        - acb-profile:/app/var/profile
      environment:
        RUST_LOG: "info,acb_daemon=debug"
        ACB_BIND: "0.0.0.0"
      restart: unless-stopped
  volumes:
    acb-profile:
  ```
- macOS host での inotify 不達対策: `config_poll_ms` をデフォ 0、env で上書き可能。

### J. クリティカルファイル (新規作成)

実装で確実に必要になるもの:

- `/home/ec/access-control-browser/Cargo.toml` — workspace
- `/home/ec/access-control-browser/crates/policy/src/url_validator.rs` — 単一の URL 判定関数
- `/home/ec/access-control-browser/crates/policy/src/config.rs` — YAML スキーマ + deny_unknown_fields
- `/home/ec/access-control-browser/crates/policy/src/element_policy.rs` — allowed_classes の解決
- `/home/ec/access-control-browser/crates/policy/src/request_policy.rs` — top-level vs subresource 判定
- `/home/ec/access-control-browser/crates/policy/tests/*.rs` — **先に書くテスト群**
- `/home/ec/access-control-browser/crates/injected-js/src/snapshot-helper.ts` — 固定 helper (isolated world)
- `/home/ec/access-control-browser/crates/injected-js/dist/snapshot-helper.js` — チェックイン済み配布物 (CI で再ビルド検証)
- `/home/ec/access-control-browser/crates/daemon/build.rs` — UI / helper の embed + ハッシュ計算
- `/home/ec/access-control-browser/crates/daemon/src/browser/launch.rs` — Chromium 起動 (hardening フラグ)
- `/home/ec/access-control-browser/crates/daemon/src/browser/interceptor.rs` — Fetch.enable と Target ガード
- `/home/ec/access-control-browser/crates/daemon/src/browser/injected.rs` — isolated world + helper 注入
- `/home/ec/access-control-browser/crates/daemon/src/snapshot/ref_table.rs` — @eN 採番と世代管理
- `/home/ec/access-control-browser/crates/daemon/src/api/{nav,actions,snapshot_route,screencast_ws,events_sse,config_route}.rs`
- `/home/ec/access-control-browser/crates/daemon/src/auth.rs` — token middleware
- `/home/ec/access-control-browser/crates/daemon/src/reload.rs` — notify + arc-swap
- `/home/ec/access-control-browser/crates/cli/src/main.rs` — clap、daemon クライアント
- `/home/ec/access-control-browser/ui/components/{viewport,location-bar,snapshot-panel,chat-panel,activity-feed}.tsx`
- `/home/ec/access-control-browser/ui/lib/url-validator.ts` — policy::url_validator の TS ポート
- `/home/ec/access-control-browser/Dockerfile`, `compose.yml`, `.dockerignore`
- `/home/ec/access-control-browser/config.example.yaml`
- `/home/ec/access-control-browser/docs/{architecture,security-model,config-schema,api}.md`

### K. マイルストーン (テストファースト)

1. **M1**: workspace skeleton + `crates/policy` 完成 (テスト先行)、`acb-cli validate <url>` だけ動く。
2. **M2**: daemon + chromiumoxide + Fetch interceptor + top-level URL ブロック + token + SSE。テスト: `auth_required`, `url_block_top_level`。
3. **M3**: サブリソース継承 + ポップアップ閉鎖 + ダウンロード拒否 + SW バイパス + WebRTC 無効。テスト: `subresource_allow`, `popup_close`, `download_deny`, `sw_bypass`。
4. **M4**: injected-js (isolated world) + element_policy + ref_table + アクション群。テスト: `element_filter`, `helper_hash`, `stale_ref_410`, `shadow_dom_open`。
5. **M5**: CLI 全コマンド (open/snapshot/click/fill/type/press/hover/select/check/find/wait/status/logs/reload)。テスト: cli integration。
6. **M6**: UI (Next.js export) + rust-embed + CSP + ChatPanel (AI SDK tool calls)。テスト: Playwright smoke。
7. **M7**: Dockerfile + compose.yml + multi-arch CI (buildx)。テスト: `docker buildx --platform linux/amd64,linux/arm64` 成功 + 起動 smoke。

各マイルストーンは CLAUDE.md に従いブランチ作成 → テスト先行 → 実装 → codex skill による code review + security review → commit + push (M1 リリース後は PR)。

---

## Verification

実装完了時に以下を実行し、すべて緑であること:

1. **policy 単体テスト**:
   ```
   cargo test -p acb-policy
   ```
   `validate_url`: regex/FQDN(subdomains true/false)/IP CIDR/ports/scheme deny/malformed の網羅、`request_policy`: top-level vs subresource、`element_policy`: allowed_classes 解決。

2. **daemon 結合テスト** (実 Chromium 使用、モック禁止):
   ```
   cargo test -p acb-daemon -- --test-threads=1
   ```
   - `auth_required`: 401 / 401 / 200
   - `url_block_top_level`: 許可 URL ロード成功、非許可 URL は `BlockedByClient` でブロック + SSE 通知
   - `subresource_allow`: 許可ページから CDN CSS/JS が実際に適用される (helper の `collect` が `getComputedStyle` 経由でスタイル反映を確認)
   - `popup_close`: `window.open` が即時クローズ
   - `download_deny`: ダウンロード発火しない
   - `sw_bypass`: Service Worker が登録されないこと、既存 SW が消えること
   - `element_filter`: 許可 class 持ち + 子孫のみが snapshot に出る
   - `stale_ref_410`: 旧世代 ref は 410
   - `helper_hash`: helper を改竄したコピーで起動 → 拒否
   - `hot_reload`: config 差し替えで in-flight が落ちず新ルール反映、malformed なら旧 policy 維持 + `reload_failed` SSE

3. **CLI 結合テスト**:
   ```
   cargo test -p acb-cli
   ```
   - `acb-cli open <blocked>` → exit 1
   - `acb-cli open <allowed>` → exit 0
   - token 無し → exit 1 "daemon not running"

4. **インジェクション grep gate** (CI):
   ```
   ! grep -RE "Runtime\\.evaluate|expression\\s*:" crates/daemon/src \
     | grep -v 'allowlisted helper bootstrap'
   ```

5. **UI smoke (Playwright)**:
   ```
   cd ui && npm run build && npm run test:e2e
   ```
   許可 URL を locaction bar に入力 → screencast フレーム到着、`javascript:` 入力 → 赤判定で submit 不可。

6. **Docker multi-arch**:
   ```
   docker buildx build --platform linux/amd64,linux/arm64 -t acb:test .
   docker run --rm acb:test acb-daemon --version
   docker compose up -d && \
     curl -s -H "Authorization: Bearer $(cat logs/token)" http://localhost:39100/healthz
   ```

7. **手動 E2E** (golden path):
   - `docker compose up`
   - ブラウザで `http://localhost:39100/` を開き、UI 表示
   - LocationBar に許可 URL → 画面に screencast、SnapshotPanel に `@eN` 一覧
   - 別タームから `acb-cli snapshot` / `acb-cli click @e1` が同セッションで動く
   - `config.yaml` を編集して非許可 URL を許可へ → ActivityFeed に `policy.reloaded`、即座に開ける
