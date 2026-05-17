"use client";

import { useEffect, useRef } from "react";

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

  // ---- WS lifecycle: open on sessionId, close on unmount / change. -----
  useEffect(() => {
    if (!sessionId || !token) return;
    const wsScheme = window.location.protocol === "https:" ? "wss" : "ws";
    const url = `${wsScheme}://${window.location.host}/sessions/${sessionId}/viewport?token=${encodeURIComponent(
      token
    )}`;
    const ws = new WebSocket(url);
    ws.binaryType = "blob";
    wsRef.current = ws;

    ws.onmessage = (ev) => {
      if (!(ev.data instanceof Blob)) return;
      const next = URL.createObjectURL(ev.data);
      const img = imgRef.current;
      if (img) {
        const prev = blobUrlRef.current;
        img.src = next;
        if (prev) URL.revokeObjectURL(prev);
        blobUrlRef.current = next;
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

  function send(obj: object) {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(obj));
    }
  }

  // ---- Mouse: translate client (CSS) coords -> page CSS coords. --------
  function toPageCoords(e: React.PointerEvent | React.MouseEvent) {
    const img = imgRef.current;
    if (!img) return { x: 0, y: 0 };
    const r = img.getBoundingClientRect();
    return {
      x: ((e.clientX - r.left) * pageWidth) / Math.max(r.width, 1),
      y: ((e.clientY - r.top) * pageHeight) / Math.max(r.height, 1),
    };
  }

  function onPointerDown(e: React.PointerEvent) {
    inputRef.current?.focus({ preventScroll: true });
    const { x, y } = toPageCoords(e);
    const button =
      e.button === 2 ? "right" : e.button === 1 ? "middle" : "left";
    send({ kind: "mouse", type: "move", x, y });
    send({ kind: "mouse", type: "down", x, y, button, click_count: 1 });
  }
  function onPointerUp(e: React.PointerEvent) {
    const { x, y } = toPageCoords(e);
    const button =
      e.button === 2 ? "right" : e.button === 1 ? "middle" : "left";
    send({ kind: "mouse", type: "up", x, y, button });
  }
  function onPointerMove(e: React.PointerEvent) {
    // Throttle mousemove a bit so we don't flood the WS.
    if (e.buttons === 0) return;
    const { x, y } = toPageCoords(e);
    send({ kind: "mouse", type: "move", x, y });
  }
  function onContextMenu(e: React.MouseEvent) {
    // Forward right-click to the page; don't show the browser's own menu.
    e.preventDefault();
  }

  // ---- Keyboard: forward when not in IME composition. ------------------
  function isComposing(e: React.KeyboardEvent) {
    return (
      (e.nativeEvent as KeyboardEvent).isComposing || e.keyCode === 229
    );
  }
  function onKeyDown(e: React.KeyboardEvent) {
    if (isComposing(e)) return;
    e.preventDefault();
    send({
      kind: "key",
      type: "down",
      key: e.key,
      code: e.code,
      text: e.key.length === 1 && !e.ctrlKey && !e.metaKey ? e.key : undefined,
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
    // Clear the local IME buffer so the next composition starts fresh.
    if (inputRef.current) inputRef.current.value = "";
  }

  return (
    <div
      style={{
        position: "relative",
        display: "inline-block",
        maxWidth: "100%",
      }}
      onContextMenu={onContextMenu}
    >
      <img
        ref={imgRef}
        alt="live viewport"
        style={{
          display: "block",
          maxWidth: "100%",
          height: "auto",
          background: "#000",
          aspectRatio: `${pageWidth} / ${pageHeight}`,
        }}
        draggable={false}
      />
      {/* Transparent textarea overlay for IME composition events. */}
      <textarea
        ref={inputRef}
        aria-hidden
        autoCapitalize="off"
        autoCorrect="off"
        spellCheck={false}
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
          // pointer events stay on the wrapper below
          pointerEvents: "none",
        }}
        onKeyDown={onKeyDown}
        onKeyUp={onKeyUp}
        onCompositionStart={onCompositionStart}
        onCompositionUpdate={onCompositionUpdate}
        onCompositionEnd={onCompositionEnd}
      />
      {/* Pointer-event capture layer. Lives above the textarea so clicks
          register here and focus the textarea programmatically. */}
      <div
        style={{ position: "absolute", inset: 0, cursor: "pointer" }}
        onPointerDown={onPointerDown}
        onPointerUp={onPointerUp}
        onPointerMove={onPointerMove}
      />
    </div>
  );
}
