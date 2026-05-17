// SPDX-License-Identifier: MIT
// Copyright (c) 2026 fuba — part of access-control-browser
// (https://github.com/fuba/access-control-browser).
//
// access-control-browser DOM helper. Lives in an isolated world so page
// scripts cannot read or override it. Exposes a frozen `__acb` object with
// only the methods the daemon calls; there is no eval surface here.
//
// Subtree-allowed semantics: an element is accessible if its classList
// contains an entry from `allowedClasses`, OR an ancestor is accessible.
// Inaccessible elements never appear in any returned snapshot.
(function () {
  "use strict";
  if (globalThis.__acb) {
    return;
  }

  const acbIdByEl = new WeakMap();
  const elByAcbId = new Map();
  let nextAcbId = 1;

  function getAcbId(el) {
    let id = acbIdByEl.get(el);
    if (id === undefined) {
      id = nextAcbId++;
      acbIdByEl.set(el, id);
      elByAcbId.set(id, el);
    }
    return id;
  }

  function descendsAccessible(el, allowed) {
    let n = el;
    while (n) {
      if (n.classList && allowed.some(function (c) { return n.classList.contains(c); })) {
        return true;
      }
      // Cross shadow roots upward via composed parent chain.
      if (n.parentNode) {
        if (n.parentNode.host) {
          n = n.parentNode.host;
        } else {
          n = n.parentNode;
        }
      } else {
        n = null;
      }
    }
    return false;
  }

  function walk(root, allowed, includeRoot, out, parentAcbId) {
    const stack = [];
    if (includeRoot) stack.push({ el: root, parent: parentAcbId });
    else if (root.childNodes) {
      const cs = root.childNodes;
      for (let i = 0; i < cs.length; i++) stack.push({ el: cs[i], parent: parentAcbId });
    }
    while (stack.length) {
      const { el, parent } = stack.pop();
      if (!el || el.nodeType !== 1) continue;
      const acc = descendsAccessible(el, allowed);
      let p = parent;
      if (acc) {
        const acbId = getAcbId(el);
        out.push(snapshotOf(el, acbId, p));
        p = acbId;
      }
      // Descend into children
      const children = el.children;
      for (let i = 0; i < children.length; i++) stack.push({ el: children[i], parent: p });
      // Descend into open shadow root if any
      if (el.shadowRoot) {
        const ch = el.shadowRoot.children;
        for (let i = 0; i < ch.length; i++) stack.push({ el: ch[i], parent: p });
      }
    }
  }

  function snapshotOf(el, acbId, parentAcbId) {
    const tag = (el.tagName || "").toLowerCase();
    const text = trim((el.textContent || ""), 200);
    const role = el.getAttribute && (el.getAttribute("role") || implicitRole(tag));
    const name = ariaName(el);
    const href = el.getAttribute && el.getAttribute("href");
    const rect = el.getBoundingClientRect ? el.getBoundingClientRect() : null;
    return {
      acbId: acbId,
      tag: tag,
      role: role || null,
      name: name || null,
      text: text,
      href: href || null,
      rect: rect ? { x: rect.x, y: rect.y, w: rect.width, h: rect.height } : null,
      parent_acb_id: parentAcbId === undefined ? null : parentAcbId,
    };
  }

  function implicitRole(tag) {
    switch (tag) {
      case "a": return "link";
      case "button": return "button";
      case "input": return "textbox";
      case "select": return "combobox";
      case "textarea": return "textbox";
      case "img": return "img";
      case "nav": return "navigation";
      case "main": return "main";
      case "header": return "banner";
      case "footer": return "contentinfo";
      default: return null;
    }
  }

  function ariaName(el) {
    if (!el.getAttribute) return null;
    return (
      el.getAttribute("aria-label") ||
      el.getAttribute("title") ||
      (el.tagName === "INPUT" ? el.getAttribute("placeholder") : null) ||
      null
    );
  }

  function trim(s, n) {
    s = (s + "").replace(/\s+/g, " ").trim();
    return s.length > n ? s.slice(0, n) + "…" : s;
  }

  function collect(allowedClasses) {
    const out = [];
    if (!document.body) return out;
    walk(document.body, allowedClasses || [], true, out, null);
    return out;
  }

  function resolve(acbId) {
    return elByAcbId.get(acbId) || null;
  }

  function findRole(role, allowedClasses) {
    const nodes = collect(allowedClasses);
    return nodes.filter(function (n) { return n.role === role; });
  }

  function findText(query, allowedClasses) {
    const nodes = collect(allowedClasses);
    const q = (query + "").toLowerCase();
    return nodes.filter(function (n) { return (n.text || "").toLowerCase().includes(q); });
  }

  function elementBox(acbId) {
    const el = resolve(acbId);
    if (!el || !el.getBoundingClientRect) return null;
    const r = el.getBoundingClientRect();
    return { x: r.x, y: r.y, w: r.width, h: r.height };
  }

  function elementText(acbId) {
    const el = resolve(acbId);
    if (!el) return null;
    return el.textContent || "";
  }

  function elementAttr(acbId, attr) {
    const el = resolve(acbId);
    if (!el || !el.getAttribute) return null;
    return el.getAttribute(attr);
  }

  function fillValue(acbId, value) {
    const el = resolve(acbId);
    if (!el) return false;
    // Use property setters so frameworks (React, Vue) see the change.
    const proto =
      el instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
    const desc = Object.getOwnPropertyDescriptor(proto, "value");
    if (desc && desc.set) {
      desc.set.call(el, value);
    } else {
      el.value = value;
    }
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  }

  function checkBox(acbId, checked) {
    const el = resolve(acbId);
    if (!el) return false;
    el.checked = !!checked;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  }

  function selectOption(acbId, value) {
    const el = resolve(acbId);
    if (!el) return false;
    el.value = value;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  }

  function clickEl(acbId) {
    const el = resolve(acbId);
    if (!el) return false;
    if (typeof el.scrollIntoView === "function") {
      el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
    }
    if (typeof el.click === "function") {
      el.click();
      return true;
    }
    return false;
  }

  function hoverEl(acbId) {
    const el = resolve(acbId);
    if (!el || !el.dispatchEvent) return false;
    el.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    el.dispatchEvent(new MouseEvent("mousemove", { bubbles: true }));
    return true;
  }

  function focusEl(acbId) {
    const el = resolve(acbId);
    if (!el || typeof el.focus !== "function") return false;
    el.focus();
    return true;
  }

  function pressKey(acbId, key) {
    const el = resolve(acbId);
    if (!el || !el.dispatchEvent) return false;
    if (typeof el.focus === "function") el.focus();
    const opts = { key: key, code: key, bubbles: true };
    el.dispatchEvent(new KeyboardEvent("keydown", opts));
    el.dispatchEvent(new KeyboardEvent("keypress", opts));
    el.dispatchEvent(new KeyboardEvent("keyup", opts));
    return true;
  }

  function typeText(acbId, text) {
    const el = resolve(acbId);
    if (!el) return false;
    if (typeof el.focus === "function") el.focus();
    const proto =
      el instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
    const desc = Object.getOwnPropertyDescriptor(proto, "value");
    const existing = el.value || "";
    const next = existing + text;
    if (desc && desc.set) desc.set.call(el, next);
    else el.value = next;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  }

  const api = Object.freeze({
    collect: collect,
    resolve: resolve,
    findRole: findRole,
    findText: findText,
    elementBox: elementBox,
    elementText: elementText,
    elementAttr: elementAttr,
    fillValue: fillValue,
    checkBox: checkBox,
    selectOption: selectOption,
    clickEl: clickEl,
    hoverEl: hoverEl,
    focusEl: focusEl,
    pressKey: pressKey,
    typeText: typeText,
  });

  Object.defineProperty(globalThis, "__acb", {
    value: api,
    writable: false,
    configurable: false,
    enumerable: false,
  });
})();
