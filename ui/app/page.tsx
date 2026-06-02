"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ConfigSummary,
  ValidateResult,
  validateUrl,
} from "../lib/url-validator";
import { Viewport } from "../components/Viewport";

type SnapshotItem = {
  ref: string;
  tag: string;
  role: string | null;
  name: string | null;
  text: string;
  href: string | null;
};

type SessionInfo = {
  id: string;
  url: string | null;
  title: string | null;
  created_at: number;
};

type FeedEvent = { type: string; [k: string]: unknown };

function getToken(): string {
  // Token is read from a query string `?token=...` so the operator can paste
  // it once when bookmarking. Never persisted by JS.
  if (typeof window === "undefined") return "";
  const qs = new URLSearchParams(window.location.search);
  return qs.get("token") || "";
}

async function authFetch(token: string, path: string, init?: RequestInit) {
  return fetch(path, {
    ...init,
    headers: {
      ...(init?.headers || {}),
      Authorization: `Bearer ${token}`,
    },
  });
}

function tabLabel(s: SessionInfo): string {
  if (s.title && s.title.trim()) return s.title;
  if (s.url && s.url.trim()) {
    try {
      return new URL(s.url).host || s.url;
    } catch {
      return s.url;
    }
  }
  return s.id.slice(0, 10);
}

export default function Home() {
  const [token, setToken] = useState("");
  const [cfg, setCfg] = useState<ConfigSummary | null>(null);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [loc, setLoc] = useState("");
  const [snap, setSnap] = useState<SnapshotItem[]>([]);
  const [feed, setFeed] = useState<FeedEvent[]>([]);
  const [validation, setValidation] = useState<ValidateResult | null>(null);
  // True once any authed request comes back 401 — the token is missing,
  // wrong, or stale (the daemon mints a fresh token on every restart). We
  // surface this full-screen instead of rendering a UI whose every API call
  // silently fails.
  const [authFailed, setAuthFailed] = useState(false);

  // Refs so async callbacks / SSE handlers read the latest values without
  // re-subscribing (and to avoid stale-closure session races).
  const activeIdRef = useRef<string | null>(null);
  useEffect(() => {
    activeIdRef.current = activeId;
  }, [activeId]);
  const locFocused = useRef(false);

  useEffect(() => {
    setToken(getToken());
  }, []);

  useEffect(() => {
    if (!token) return;
    authFetch(token, "/config")
      .then((r) => {
        if (r.status === 401) {
          setAuthFailed(true);
          return Promise.reject(401);
        }
        return r.ok ? r.json() : Promise.reject(r.status);
      })
      .then((c) => {
        setAuthFailed(false);
        setCfg(c);
      })
      .catch(() => setCfg(null));
  }, [token]);

  const refreshSessions = useCallback(async () => {
    if (!token) return;
    const r = await authFetch(token, "/sessions");
    if (r.status === 401) {
      setAuthFailed(true);
      return;
    }
    if (!r.ok) return;
    const list: SessionInfo[] = await r.json();
    setSessions(list);
    // Pick an active tab if we don't have a (still-valid) one.
    const cur = activeIdRef.current;
    if (!cur || !list.some((s) => s.id === cur)) {
      const next = list[0]?.id ?? null;
      setActiveId(next);
      const u = list.find((s) => s.id === next)?.url ?? "";
      setLoc(u || "");
    }
  }, [token]);

  useEffect(() => {
    if (token) refreshSessions();
  }, [token, refreshSessions]);

  // SSE: live activity + session/url updates.
  useEffect(() => {
    if (!token) return;
    const es = new EventSource(`/events?token=${encodeURIComponent(token)}`);
    es.onmessage = (e) => {
      let ev: FeedEvent;
      try {
        ev = JSON.parse(e.data) as FeedEvent;
      } catch {
        return;
      }
      setFeed((f) => [...f.slice(-200), ev]);
      if (ev.type === "session_opened" || ev.type === "session_closed") {
        refreshSessions();
      } else if (ev.type === "session_url") {
        const sid = ev.session as string;
        const url = (ev.url as string) || "";
        const title = (ev.title as string | null) ?? null;
        setSessions((list) =>
          list.map((s) => (s.id === sid ? { ...s, url, title } : s))
        );
        // Reflect the navigated URL in the location bar of the active tab,
        // unless the operator is mid-edit.
        if (sid === activeIdRef.current && !locFocused.current) {
          setLoc(url);
        }
      }
    };
    return () => es.close();
  }, [token, refreshSessions]);

  useEffect(() => {
    if (!cfg || !loc.trim()) {
      setValidation(null);
      return;
    }
    setValidation(validateUrl(loc.trim(), cfg));
  }, [loc, cfg]);

  const ensureActive = useCallback(async (): Promise<string | null> => {
    if (activeIdRef.current) return activeIdRef.current;
    const r = await authFetch(token, "/sessions", { method: "POST" });
    if (!r.ok) return null;
    const v = await r.json();
    activeIdRef.current = v.id;
    setActiveId(v.id);
    await refreshSessions();
    return v.id as string;
  }, [token, refreshSessions]);

  const newTab = useCallback(async () => {
    const r = await authFetch(token, "/sessions", { method: "POST" });
    if (!r.ok) return;
    const v = await r.json();
    activeIdRef.current = v.id;
    setActiveId(v.id);
    setLoc("");
    await refreshSessions();
  }, [token, refreshSessions]);

  const closeTab = useCallback(
    async (id: string) => {
      await authFetch(token, `/sessions/${id}`, { method: "DELETE" });
      if (activeIdRef.current === id) {
        activeIdRef.current = null;
        setActiveId(null);
      }
      await refreshSessions();
    },
    [token, refreshSessions]
  );

  const switchTo = useCallback(
    (id: string) => {
      activeIdRef.current = id;
      setActiveId(id);
      const u = sessions.find((s) => s.id === id)?.url ?? "";
      setLoc(u || "");
      setSnap([]);
    },
    [sessions]
  );

  const takeSnapshot = useCallback(async () => {
    const id = activeIdRef.current;
    if (!id) return;
    const r = await authFetch(token, `/sessions/${id}/snapshot`, {
      method: "POST",
    });
    if (!r.ok) return;
    const v = await r.json();
    setSnap(v.refs || []);
  }, [token]);

  const open = useCallback(async () => {
    const id = await ensureActive();
    if (!id) return;
    const r = await authFetch(token, `/sessions/${id}/open`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url: loc.trim() }),
    });
    if (!r.ok) {
      const body = await r.text();
      setFeed((f) => [
        ...f,
        {
          type: "blocked",
          ts: Date.now() / 1000,
          session: id,
          url: loc,
          reason: body,
          kind: "open-rejected",
        },
      ]);
    } else {
      setTimeout(takeSnapshot, 500);
    }
  }, [ensureActive, takeSnapshot, token, loc]);

  const nav = useCallback(
    async (verb: "back" | "forward" | "reload") => {
      const id = activeIdRef.current;
      if (!id) return;
      await authFetch(token, `/sessions/${id}/${verb}`, { method: "POST" });
      setTimeout(takeSnapshot, 500);
    },
    [token, takeSnapshot]
  );

  const submitDisabled = useMemo(
    () => !cfg || !validation || !validation.ok || !loc.trim(),
    [cfg, validation, loc]
  );
  const klass = !loc ? "" : validation?.ok ? "ok" : "bad";

  if (!token || authFailed) {
    const expired = authFailed;
    return (
      <main className="app">
        <div
          style={{
            position: "fixed",
            inset: 0,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            padding: "2rem",
            background: "var(--bg)",
          }}
        >
          <div
            style={{
              maxWidth: 560,
              border: `1px solid ${expired ? "var(--bad)" : "var(--border)"}`,
              borderRadius: 10,
              padding: "1.5rem 1.75rem",
              background: "var(--panel)",
            }}
          >
            <h1 style={{ marginTop: 0, color: expired ? "var(--bad)" : undefined }}>
              {expired ? "🔒 認証に失敗しました (401)" : "access-control-browser"}
            </h1>
            {expired ? (
              <>
                <p>
                  トークンが無効か期限切れです。デーモンは<b>起動するたびに新しい
                  トークンを生成する</b>ため、再起動後は URL のトークンが古くなります。
                </p>
                <p>
                  ホスト上の最新トークンで開き直してください:
                  <br />
                  <code>
                    {typeof window !== "undefined"
                      ? `${window.location.origin}/?token=<最新トークン>`
                      : "/?token=<最新トークン>"}
                  </code>
                </p>
                <p style={{ color: "var(--muted)", fontSize: 13 }}>
                  トークンは daemon ホストの{" "}
                  <code>$XDG_RUNTIME_DIR/access-control-browser.token</code>{" "}
                  （または <code>--token-file</code> で指定したパス）にあります。
                </p>
              </>
            ) : (
              <p>
                URL に <code>?token=&lt;your-token&gt;</code> を付けてください。
                トークンは daemon ホストの{" "}
                <code>$XDG_RUNTIME_DIR/access-control-browser.token</code> にあります。
              </p>
            )}
          </div>
        </div>
      </main>
    );
  }

  return (
    <main className="app">
      {/* Tab bar */}
      <div className="tabbar">
        {sessions.map((s) => (
          <div
            key={s.id}
            className={"tab" + (s.id === activeId ? " active" : "")}
            onClick={() => switchTo(s.id)}
            title={s.url || s.id}
          >
            <span className="tab-label">{tabLabel(s)}</span>
            <button
              className="tab-copy"
              title={`copy session id: ${s.id}`}
              onClick={(e) => {
                e.stopPropagation();
                navigator.clipboard?.writeText(s.id);
              }}
            >
              ⧉
            </button>
            <button
              className="tab-close"
              title="close tab"
              onClick={(e) => {
                e.stopPropagation();
                closeTab(s.id);
              }}
            >
              ×
            </button>
          </div>
        ))}
        <button className="tab-new" title="new tab" onClick={newTab}>
          +
        </button>
      </div>

      {/* Nav controls + location bar */}
      <div className="topbar">
        <button className="navbtn" title="back" onClick={() => nav("back")} disabled={!activeId}>
          ‹
        </button>
        <button
          className="navbtn"
          title="forward"
          onClick={() => nav("forward")}
          disabled={!activeId}
        >
          ›
        </button>
        <button
          className="navbtn"
          title="reload"
          onClick={() => nav("reload")}
          disabled={!activeId}
        >
          ⟳
        </button>
        <input
          className={klass}
          placeholder="https://github.com/..."
          value={loc}
          onChange={(e) => setLoc(e.target.value)}
          onFocus={() => (locFocused.current = true)}
          onBlur={() => (locFocused.current = false)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !submitDisabled) open();
          }}
        />
        <button onClick={open} disabled={submitDisabled}>
          Open
        </button>
        <button onClick={takeSnapshot} disabled={!activeId}>
          Snapshot
        </button>
      </div>

      <div className="body">
        <div className="viewport">
          {activeId ? (
            <Viewport
              sessionId={activeId}
              token={token}
              pageWidth={cfg?.viewport?.width}
              pageHeight={cfg?.viewport?.height}
            />
          ) : (
            <div className="placeholder">
              <p>
                Click <b>+</b> to open a tab, type an allowed URL, and press{" "}
                <kbd>Enter</kbd>.
              </p>
              {cfg && (
                <p>
                  helper sha256:{" "}
                  <code style={{ fontSize: 11 }}>{cfg.helper_sha256}</code>
                </p>
              )}
            </div>
          )}
        </div>
        <div className="side">
          <div className="panel">
            <h2>Snapshot ({snap.length})</h2>
            <ul className="refs">
              {snap.map((s) => (
                <li key={s.ref}>
                  <span className="ref">{s.ref}</span>
                  <span className="role">{s.role || s.tag}</span>
                  <span className="text">{s.text || s.name || ""}</span>
                </li>
              ))}
            </ul>
          </div>
          <div className="panel">
            <h2>Activity</h2>
            <div className="feed">
              {feed
                .slice()
                .reverse()
                .map((ev, i) => (
                  <div key={i}>
                    <span className="time">
                      {new Date(((ev.ts as number) || 0) * 1000).toLocaleTimeString()}
                    </span>
                    <span
                      className={
                        ev.type === "blocked"
                          ? "b"
                          : ev.type === "navigated" || ev.type === "session_url"
                          ? "a"
                          : "r"
                      }
                    >
                      {ev.type}
                    </span>{" "}
                    {(ev.url as string) ||
                      (ev.etag as string) ||
                      (ev.error as string) ||
                      ""}
                  </div>
                ))}
            </div>
          </div>
        </div>
      </div>
    </main>
  );
}
