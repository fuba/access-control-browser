"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ConfigSummary,
  ValidateResult,
  validateUrl,
} from "../lib/url-validator";

type SnapshotItem = {
  ref: string;
  tag: string;
  role: string | null;
  name: string | null;
  text: string;
  href: string | null;
};

type FeedEvent =
  | { type: "navigated"; ts: number; session: string; url: string; rule: string }
  | { type: "blocked"; ts: number; session: string; url: string; reason: string; kind: string }
  | { type: "policy_reloaded"; ts: number; etag: string }
  | { type: "policy_reload_failed"; ts: number; error: string }
  | { type: string; [k: string]: unknown };

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

export default function Home() {
  const [token, setToken] = useState("");
  const [cfg, setCfg] = useState<ConfigSummary | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [url, setUrl] = useState("");
  const [snap, setSnap] = useState<SnapshotItem[]>([]);
  const [feed, setFeed] = useState<FeedEvent[]>([]);
  const [validation, setValidation] = useState<ValidateResult | null>(null);

  useEffect(() => {
    setToken(getToken());
  }, []);

  useEffect(() => {
    if (!token) return;
    authFetch(token, "/config")
      .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
      .then(setCfg)
      .catch(() => setCfg(null));
  }, [token]);

  useEffect(() => {
    if (!token) return;
    const es = new EventSource(`/events?token=${encodeURIComponent(token)}`);
    es.onmessage = (e) => {
      try {
        const ev = JSON.parse(e.data) as FeedEvent;
        setFeed((f) => [...f.slice(-200), ev]);
      } catch {
        /* ignore */
      }
    };
    return () => es.close();
  }, [token]);

  useEffect(() => {
    if (!cfg) {
      setValidation(null);
      return;
    }
    if (!url.trim()) {
      setValidation(null);
      return;
    }
    setValidation(validateUrl(url.trim(), cfg));
  }, [url, cfg]);

  const ensureSession = useCallback(async () => {
    if (sessionId) return sessionId;
    const r = await authFetch(token, "/sessions", { method: "POST" });
    if (!r.ok) return null;
    const v = await r.json();
    setSessionId(v.id);
    return v.id as string;
  }, [token, sessionId]);

  const open = useCallback(async () => {
    const id = await ensureSession();
    if (!id) return;
    const r = await authFetch(token, `/sessions/${id}/open`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url: url.trim() }),
    });
    if (!r.ok) {
      const body = await r.text();
      setFeed((f) => [
        ...f,
        {
          type: "blocked",
          ts: Date.now() / 1000,
          session: id,
          url,
          reason: body,
          kind: "open-rejected",
        } as FeedEvent,
      ]);
    } else {
      // small delay so the page has time to paint
      setTimeout(takeSnapshot, 500);
    }
  }, [ensureSession, token, url]);

  const takeSnapshot = useCallback(async () => {
    const id = sessionId || (await ensureSession());
    if (!id) return;
    const r = await authFetch(token, `/sessions/${id}/snapshot`, {
      method: "POST",
    });
    if (!r.ok) return;
    const v = await r.json();
    setSnap(v.refs || []);
  }, [ensureSession, sessionId, token]);

  const submitDisabled = useMemo(
    () => !cfg || !validation || !validation.ok || !url.trim(),
    [cfg, validation, url]
  );
  const klass = !url
    ? ""
    : validation?.ok
    ? "ok"
    : "bad";

  if (!token) {
    return (
      <main className="app">
        <div style={{ padding: "2rem", maxWidth: 720 }}>
          <h1>access-control-browser</h1>
          <p>
            Append <code>?token=&lt;your-token&gt;</code> to the URL. The token
            is in <code>$XDG_RUNTIME_DIR/access-control-browser.token</code> on
            the daemon host.
          </p>
        </div>
      </main>
    );
  }

  return (
    <main className="app">
      <div className="topbar">
        <input
          className={klass}
          placeholder="https://github.com/..."
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !submitDisabled) open();
          }}
        />
        <button onClick={open} disabled={submitDisabled}>
          Open
        </button>
        <button onClick={takeSnapshot} disabled={!sessionId}>
          Snapshot
        </button>
      </div>
      <div className="body">
        <div className="viewport">
          <div className="placeholder">
            <p>Live viewport (Page.startScreencast) — planned for v0.2.</p>
            <p>
              For v0.1 the operator drives the browser via this UI and the
              CLI; the page renders headless inside the daemon. Use Snapshot
              to inspect what the agent sees.
            </p>
            {cfg && (
              <p>
                helper sha256:{" "}
                <code style={{ fontSize: 11 }}>{cfg.helper_sha256}</code>
              </p>
            )}
          </div>
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
                      {new Date((ev as any).ts * 1000).toLocaleTimeString()}
                    </span>
                    <span
                      className={
                        ev.type === "blocked"
                          ? "b"
                          : ev.type === "navigated"
                          ? "a"
                          : "r"
                      }
                    >
                      {ev.type}
                    </span>{" "}
                    {(ev as any).url ||
                      (ev as any).etag ||
                      (ev as any).error ||
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
