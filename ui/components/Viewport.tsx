"use client";

import { useEffect, useRef, useState } from "react";

type Props = {
  sessionId: string | null;
  token: string;
  /** Logical page size used to scale mouse coords. The daemon ships JPEGs
   *  rendered at this size; if you change the chromium viewport, change
   *  this too. */
  pageWidth?: number;
  pageHeight?: number;
};

// Modifier mask matching CDP's `Input.dispatchKeyEvent` bit field:
// Alt=1, Ctrl=2, Meta/Command=4, Shift=8.
function mods(e: React.KeyboardEvent | KeyboardEvent): number {
  return (
    (e.altKey ? 1 : 0) |
    (e.ctrlKey ? 2 : 0) |
    (e.metaKey ? 4 : 0) |
    (e.shiftKey ? 8 : 0)
  );
}

type WsState = "connecting" | "open" | "closed" | "error";

export function Viewport({
  sessionId,
  token,
  pageWidth = 1280,
  pageHeight = 800,
}: Props) {
  const imgRef = useRef<HTMLImageElement | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const blobUrlRef = useRef<string | null>(null);

  const [wsState, setWsState] = useState<WsState>("connecting");
  const [frames, setFrames] = useState(0);
  const [lastSent, setLastSent] = useState<string>("(none yet)");

  // ---- WS lifecycle: open on sessionId, close on unmount / change. -----
  useEffect(() => {
    if (!sessionId || !token) return;
    setWsState("connecting");
    setFrames(0);
    const wsScheme = window.location.protocol === "https:" ? "wss" : "ws";
    const url = `${wsScheme}://${window.location.host}/sessions/${sessionId}/viewport?token=${encodeURIComponent(
      token
    )}`;
    const ws = new WebSocket(url);
    ws.binaryType = "blob";
    wsRef.current = ws;

    ws.onopen = () => setWsState("open");
    ws.onclose = () => setWsState("closed");
    ws.onerror = () => setWsState("error");

    ws.onmessage = (ev) => {
      if (!(ev.data instanceof Blob)) return;
      // Binary WS frames arrive with Blob.type === "". Some browsers
      // refuse to decode an <img src="blob:..."> URL without a sniff-able
      // MIME (especially under headless / strict CSP), and the image
      // renders as a blank box even though the bytes are a valid JPEG.
      // Re-wrap with an explicit image/jpeg type so the browser always
      // decodes.
      const blob =
        ev.data.type === "image/jpeg"
          ? ev.data
          : new Blob([ev.data], { type: "image/jpeg" });
      const next = URL.createObjectURL(blob);
      const img = imgRef.current;
      if (img) {
        const prev = blobUrlRef.current;
        img.src = next;
        if (prev) URL.revokeObjectURL(prev);
        blobUrlRef.current = next;
        setFrames((n) => n + 1);
      } else {
        URL.revokeObjectURL(next);
      }
    };

    return () => {
      ws.close();
      wsRef.current = null;
      if (blobUrlRef.current) URL.revokeObjectURL(blobUrlRef.current);
      blobUrlRef.current = null;
    };
  }, [sessionId, token]);

  function send(obj: { kind: string } & Record<string, unknown>) {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(obj));
      setLastSent(`${obj.kind}: ${JSON.stringify(obj).slice(0, 80)}`);
    } else {
      setLastSent(`(dropped, ws=${ws?.readyState ?? "null"}) ${obj.kind}`);
    }
  }

  // ---- Mouse: translate client (CSS) coords -> page CSS coords. --------
  // Important: scale on a rect width/height of at least 1 to avoid huge
  // coords before the first frame paints the <img>.
  function toPageCoords(e: React.PointerEvent | React.MouseEvent) {
    const img = imgRef.current;
    if (!img) return { x: 0, y: 0 };
    const r = img.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) {
      // The <img> hasn't laid out yet; clicking would map to garbage.
      return { x: -1, y: -1 };
    }
    return {
      x: ((e.clientX - r.left) * pageWidth) / r.width,
      y: ((e.clientY - r.top) * pageHeight) / r.height,
    };
  }

  function onPointerDown(e: React.PointerEvent) {
    inputRef.current?.focus({ preventScroll: true });
    const { x, y } = toPageCoords(e);
    if (x < 0) return;
    const button =
      e.button === 2 ? "right" : e.button === 1 ? "middle" : "left";
    send({ kind: "mouse", type: "move", x, y });
    send({ kind: "mouse", type: "down", x, y, button, click_count: 1 });
  }
  function onPointerUp(e: React.PointerEvent) {
    const { x, y } = toPageCoords(e);
    if (x < 0) return;
    const button =
      e.button === 2 ? "right" : e.button === 1 ? "middle" : "left";
    send({ kind: "mouse", type: "up", x, y, button });
  }
  function onPointerMove(e: React.PointerEvent) {
    if (e.buttons === 0) return;
    const { x, y } = toPageCoords(e);
    if (x < 0) return;
    send({ kind: "mouse", type: "move", x, y });
  }
  function onContextMenu(e: React.MouseEvent) {
    e.preventDefault();
  }

  // ---- Keyboard: forward when not in IME composition. ------------------
  function isComposing(e: React.KeyboardEvent) {
    return (
      (e.nativeEvent as KeyboardEvent).isComposing || e.keyCode === 229
    );
  }
  function keyText(e: React.KeyboardEvent): string | undefined {
    // Only treat single-char keys (ASCII printable, Latin-1, etc.) as text
    // for the `text` field. Don't forward Enter / Tab / Escape / arrows
    // as "text" — they're identified via `key` / `code` and the page
    // handles them as keystrokes.
    if (e.ctrlKey || e.metaKey) return undefined;
    if (e.key.length === 1) return e.key;
    if (e.key === "Enter") return "\r";
    return undefined;
  }
  function onKeyDown(e: React.KeyboardEvent) {
    if (isComposing(e)) return;
    e.preventDefault();
    send({
      kind: "key",
      type: "down",
      key: e.key,
      code: e.code,
      text: keyText(e),
      modifiers: mods(e),
    });
  }
  function onKeyUp(e: React.KeyboardEvent) {
    if (isComposing(e)) return;
    e.preventDefault();
    send({
      kind: "key",
      type: "up",
      key: e.key,
      code: e.code,
      modifiers: mods(e),
    });
  }

  // ---- IME composition: open / update / commit. ------------------------
  function onCompositionStart() {
    send({ kind: "composition_start" });
  }
  function onCompositionUpdate(e: React.CompositionEvent) {
    const text = e.data ?? "";
    send({
      kind: "composition_update",
      text,
      selection_start: text.length,
      selection_end: text.length,
    });
  }
  function onCompositionEnd(e: React.CompositionEvent) {
    send({ kind: "composition_end", text: e.data ?? "" });
    if (inputRef.current) inputRef.current.value = "";
  }

  return (
    <div style={{ width: "100%", display: "flex", flexDirection: "column" }}>
      {/* Diagnostic strip so the operator can see whether the WS / image
          / input pipeline is alive. */}
      <div
        style={{
          fontFamily: "ui-monospace, SF Mono, Menlo, monospace",
          fontSize: 11,
          color: "var(--muted)",
          padding: "4px 8px",
          background: "var(--bg)",
          borderBottom: "1px solid var(--border)",
          display: "flex",
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <span>
          ws:{" "}
          <b
            style={{
              color:
                wsState === "open"
                  ? "var(--ok)"
                  : wsState === "error"
                  ? "var(--bad)"
                  : "var(--accent)",
            }}
          >
            {wsState}
          </b>
        </span>
        <span>frames: {frames}</span>
        <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
          last: {lastSent}
        </span>
        <span data-acb-sid={sessionId || ""} style={{ opacity: 0.6 }}>
          sid: {sessionId ? sessionId.slice(0, 12) + "…" : "(none)"}
        </span>
      </div>

      <div
        style={{
          position: "relative",
          width: "100%",
          // Reserve box at the page aspect ratio so the <img> always has
          // non-zero layout, even before the first frame.
          aspectRatio: `${pageWidth} / ${pageHeight}`,
          background: "#000",
        }}
        onContextMenu={onContextMenu}
      >
        <img
          ref={imgRef}
          alt="live viewport"
          style={{
            position: "absolute",
            inset: 0,
            width: "100%",
            height: "100%",
            display: "block",
            objectFit: "contain",
          }}
          draggable={false}
        />
        {/* IME / keyboard capture. pointer-events:none so the layer below
            still gets clicks; focus() is called programmatically. */}
        <textarea
          ref={inputRef}
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          tabIndex={0}
          style={{
            position: "absolute",
            inset: 0,
            width: "100%",
            height: "100%",
            opacity: 0,
            border: 0,
            outline: 0,
            resize: "none",
            background: "transparent",
            caretColor: "transparent",
            pointerEvents: "none",
          }}
          onKeyDown={onKeyDown}
          onKeyUp={onKeyUp}
          onCompositionStart={onCompositionStart}
          onCompositionUpdate={onCompositionUpdate}
          onCompositionEnd={onCompositionEnd}
        />
        {/* Pointer-event capture. Always on top so clicks register here;
            we forward focus to the textarea explicitly. */}
        <div
          style={{ position: "absolute", inset: 0, cursor: "crosshair" }}
          onPointerDown={onPointerDown}
          onPointerUp={onPointerUp}
          onPointerMove={onPointerMove}
        />
      </div>
    </div>
  );
}
