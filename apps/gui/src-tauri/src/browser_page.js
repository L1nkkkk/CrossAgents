function drive(action) {
  try {
    const TAG = "__crossagentsSnapshotRefs";
    const MAX_REFS = 300;
    const INTERACTIVE = [
      "a[href]",
      "button",
      "input:not([type=hidden])",
      "textarea",
      "select",
      "summary",
      "[contenteditable='']",
      "[contenteditable='true']",
      "[contenteditable='plaintext-only']",
      "[role=button]",
      "[role=link]",
      "[role=tab]",
      "[role=menuitem]",
      "[role=menuitemcheckbox]",
      "[role=menuitemradio]",
      "[role=option]",
      "[role=checkbox]",
      "[role=radio]",
      "[role=switch]",
      "[role=combobox]",
      "[role=textbox]",
      "[role=searchbox]",
      "[role=slider]",
      "[role=spinbutton]",
      "[role=treeitem]",
    ].join(", ");

    const clean = (value, max = 120) =>
      String(value ?? "")
        .replace(/\s+/g, " ")
        .trim()
        .slice(0, max);

    const winOf = (node) => node?.ownerDocument?.defaultView || window;

    const isVisible = (el) => {
      if (!el || el.nodeType !== 1 || !el.isConnected) return false;
      if (el.hidden || el.inert) return false;
      if (el.closest?.("[aria-hidden='true'], [hidden], [inert]")) return false;
      const win = winOf(el);
      const style = win.getComputedStyle?.(el);
      if (style && (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse")) {
        return false;
      }
      const rects = el.getClientRects?.();
      if (!rects || rects.length === 0) return false;
      const box = el.getBoundingClientRect();
      return box.width > 0 || box.height > 0;
    };

    const isDisabled = (el) => Boolean(el?.disabled || el?.getAttribute?.("aria-disabled") === "true");

    const isTextControl = (el) => {
      if (!el || el.nodeType !== 1) return false;
      const win = winOf(el);
      if (el instanceof win.HTMLTextAreaElement) return true;
      if (el instanceof win.HTMLInputElement) {
        const type = (el.type || "text").toLowerCase();
        return !["checkbox", "radio", "button", "submit", "reset", "image", "file", "range", "color"].includes(type);
      }
      return Boolean(el.isContentEditable);
    };

    const isScrollable = (el) => {
      if (!el || el.nodeType !== 1 || !isVisible(el)) return false;
      const win = winOf(el);
      const oy = win.getComputedStyle?.(el)?.overflowY;
      return (oy === "auto" || oy === "scroll" || oy === "overlay") && el.scrollHeight > el.clientHeight + 2;
    };

    const findPrimaryScroller = () => {
      const se = document.scrollingElement || document.documentElement;
      const winMax = Math.max(0, (se?.scrollHeight || 0) - window.innerHeight);
      if (winMax > 2) {
        return { el: null, y: Math.round(window.scrollY), max: Math.round(winMax) };
      }
      let best = null;
      let bestArea = 0;
      const queue = document.body ? [document.body] : [];
      let visited = 0;
      while (queue.length && visited < 1500) {
        const cur = queue.shift();
        visited++;
        if (cur !== document.body && isScrollable(cur)) {
          const rect = cur.getBoundingClientRect();
          if (rect.width >= 100 && rect.height >= 100) {
            const area = rect.width * rect.height;
            if (area > bestArea) {
              bestArea = area;
              best = cur;
            }
          }
        }
        if (cur.shadowRoot) {
          for (const child of cur.shadowRoot.children) queue.push(child);
        }
        for (const child of cur.children) queue.push(child);
      }
      if (best) {
        return {
          el: best,
          y: Math.round(best.scrollTop),
          max: Math.max(0, Math.round(best.scrollHeight - best.clientHeight)),
        };
      }
      return { el: null, y: Math.round(window.scrollY), max: Math.round(winMax) };
    };

    const querySelectorDeep = (root, selector, depth = 0) => {
      if (!root || depth > 8) return null;
      const direct = root.querySelector?.(selector);
      if (direct) return direct;
      const all = root.querySelectorAll?.("*") || [];
      for (const el of all) {
        if (el.shadowRoot) {
          const hit = querySelectorDeep(el.shadowRoot, selector, depth + 1);
          if (hit) return hit;
        }
        if (el.tagName === "IFRAME") {
          try {
            const doc = el.contentDocument;
            if (doc) {
              const hit = querySelectorDeep(doc, selector, depth + 1);
              if (hit) return hit;
            }
          } catch {}
        }
      }
      return null;
    };

    const queryVisibleSelectorDeep = (root, selector, depth = 0) => {
      if (!root || depth > 8) return null;
      const direct = root.querySelectorAll?.(selector) || [];
      for (const match of direct) {
        if (isVisible(match)) return match;
      }
      const all = root.querySelectorAll?.("*") || [];
      for (const el of all) {
        if (el.shadowRoot) {
          const hit = queryVisibleSelectorDeep(el.shadowRoot, selector, depth + 1);
          if (hit) return hit;
        }
        if (el.tagName === "IFRAME") {
          try {
            const doc = el.contentDocument;
            if (doc && isVisible(el)) {
              const hit = queryVisibleSelectorDeep(doc, selector, depth + 1);
              if (hit) return hit;
            }
          } catch {}
        }
      }
      return null;
    };

    const walkInteractive = (root, out, state, depth = 0) => {
      if (!root || depth > 10) return;
      const stack = [];
      const children = root.children || [];
      for (let i = children.length - 1; i >= 0; i--) stack.push(children[i]);
      while (stack.length) {
        const el = stack.pop();
        if (!el || el.nodeType !== 1) continue;
        if (el.hidden || el.inert || el.getAttribute("aria-hidden") === "true") continue;
        const win = winOf(el);
        const style = win.getComputedStyle?.(el);
        if (style && style.display === "none") continue;

        if (el.matches?.(INTERACTIVE) && isVisible(el)) {
          state.total++;
          if (out.length < MAX_REFS) out.push(el);
        }
        if (el.shadowRoot) {
          walkInteractive(el.shadowRoot, out, state, depth + 1);
        }
        if (el.tagName === "IFRAME" && isVisible(el)) {
          try {
            const doc = el.contentDocument;
            if (doc?.body) walkInteractive(doc.body, out, state, depth + 1);
          } catch {}
        }
        const kids = el.children;
        for (let i = kids.length - 1; i >= 0; i--) stack.push(kids[i]);
      }
    };

    const collectVisibleText = (root) => {
      const parts = [];
      const base = root?.innerText || root?.textContent || "";
      if (base.trim()) parts.push(base);
      const iframes = root?.querySelectorAll?.("iframe") || [];
      for (const frame of iframes) {
        if (!isVisible(frame)) continue;
        try {
          const text = frame.contentDocument?.body?.innerText || "";
          if (text.trim()) parts.push(text);
        } catch {}
      }
      return parts
        .join("\n\n")
        .replace(/[ \t]+\n/g, "\n")
        .replace(/\n{3,}/g, "\n\n")
        .trim();
    };

    const labelTextFor = (el) => {
      const root = el.getRootNode?.() || el.ownerDocument || document;
      const labelledBy = el.getAttribute?.("aria-labelledby");
      if (labelledBy) {
        const pieces = labelledBy
          .trim()
          .split(/\s+/)
          .map((id) => {
            const target = root.getElementById?.(id) || el.ownerDocument?.getElementById?.(id);
            return target ? clean(target.innerText || target.textContent, 120) : "";
          })
          .filter(Boolean);
        if (pieces.length) return clean(pieces.join(" "), 120);
      }
      const aria = el.getAttribute?.("aria-label");
      if (aria && aria.trim()) return clean(aria, 120);
      if (el.labels && el.labels.length) {
        const text = Array.from(el.labels)
          .map((label) => {
            const clone = label.cloneNode(true);
            clone.querySelectorAll?.("input, textarea, select").forEach((c) => c.remove());
            return clean(clone.textContent, 120);
          })
          .filter(Boolean)
          .join(" ");
        if (text) return clean(text, 120);
      }
      const tag = el.tagName?.toLowerCase();
      if (tag === "input") {
        const type = (el.getAttribute("type") || "text").toLowerCase();
        if (type === "button" || type === "submit" || type === "reset") {
          const val = clean(el.value, 120);
          if (val) return val;
        }
      }
      if (tag !== "select" && tag !== "input" && tag !== "textarea") {
        const text = clean(el.innerText || el.textContent, 120);
        if (text) return text;
      }
      const fallback =
        el.getAttribute?.("placeholder") ||
        el.getAttribute?.("title") ||
        el.querySelector?.("img[alt]")?.getAttribute("alt") ||
        el.querySelector?.("svg title")?.textContent ||
        el.getAttribute?.("name") ||
        "";
      return clean(fallback, 120);
    };

    const describeNode = (el, index) => {
      const tag = el.tagName.toLowerCase();
      const roleAttr = el.getAttribute("role");
      let kind = roleAttr || tag;
      if (!roleAttr && tag === "input") {
        kind = `input[${(el.getAttribute("type") || "text").toLowerCase()}]`;
      }
      const name = labelTextFor(el);
      const parts = [`[ref=${index + 1}]`, kind];
      if (name) parts.push(JSON.stringify(name));

      const win = winOf(el);
      if (el instanceof win.HTMLInputElement) {
        const type = (el.type || "text").toLowerCase();
        if (type === "checkbox" || type === "radio") {
          parts.push(el.checked ? "checked" : "unchecked");
        } else if (type === "password") {
          if (el.value) parts.push('value="••••"');
        } else if (!["button", "submit", "reset", "image"].includes(type) && el.value) {
          parts.push(`value=${JSON.stringify(clean(el.value, 80))}`);
        }
      } else if (el instanceof win.HTMLTextAreaElement) {
        if (el.value) parts.push(`value=${JSON.stringify(clean(el.value, 80))}`);
      } else if (el instanceof win.HTMLSelectElement) {
        const selected = el.selectedOptions?.[0];
        if (selected) {
          parts.push(`selected=${JSON.stringify(clean(selected.textContent || selected.value, 60))}`);
        }
        const opts = Array.from(el.options || []).filter((o) => !o.disabled);
        if (opts.length) {
          const shown = opts.slice(0, 12).map((o) => {
            const label = clean(o.textContent, 40);
            const val = clean(o.value, 40);
            return label && val && label !== val ? `${JSON.stringify(label)}(${val})` : JSON.stringify(label || val);
          });
          if (opts.length > 12) shown.push(`+${opts.length - 12} more`);
          parts.push(`options=[${shown.join(", ")}]`);
        }
      } else if (el.isContentEditable) {
        const val = clean(el.innerText || el.textContent, 80);
        if (val && val !== name) parts.push(`value=${JSON.stringify(val)}`);
      }

      const ariaChecked = el.getAttribute("aria-checked");
      if (ariaChecked === "true") parts.push("checked");
      else if (ariaChecked === "false") parts.push("unchecked");
      else if (ariaChecked === "mixed") parts.push("mixed");

      if (el.getAttribute("aria-selected") === "true") parts.push("selected");
      const expanded = el.getAttribute("aria-expanded");
      if (expanded === "true") parts.push("expanded");
      else if (expanded === "false") parts.push("collapsed");
      if (el.getAttribute("aria-pressed") === "true") parts.push("pressed");
      if (isDisabled(el)) parts.push("disabled");
      if (el.readOnly || el.getAttribute("aria-readonly") === "true") parts.push("readonly");
      if (el.required || el.getAttribute("aria-required") === "true") parts.push("required");

      if (tag === "a") {
        const href = clean(el.getAttribute("href"), 120);
        if (href && !/^javascript:/i.test(href)) parts.push(`href=${JSON.stringify(href)}`);
      }
      return parts.join(" ");
    };

    const deepActiveElement = () => {
      let active = document.activeElement;
      let guard = 0;
      while (active && guard++ < 10) {
        if (active.shadowRoot?.activeElement) {
          active = active.shadowRoot.activeElement;
          continue;
        }
        if (active.tagName === "IFRAME") {
          try {
            if (active.contentDocument?.activeElement) {
              active = active.contentDocument.activeElement;
              continue;
            }
          } catch {}
        }
        break;
      }
      return active || document.body;
    };

    const parseKeyChord = (raw) => {
      const tokens = String(raw || "")
        .split("+")
        .map((t) => t.trim())
        .filter(Boolean);
      const mods = { ctrlKey: false, altKey: false, shiftKey: false, metaKey: false };
      let keyToken = "";
      for (let i = 0; i < tokens.length; i++) {
        const token = tokens[i];
        const lower = token.toLowerCase();
        if (i < tokens.length - 1) {
          if (lower === "control" || lower === "ctrl") { mods.ctrlKey = true; continue; }
          if (lower === "alt" || lower === "option") { mods.altKey = true; continue; }
          if (lower === "shift") { mods.shiftKey = true; continue; }
          if (lower === "meta" || lower === "cmd" || lower === "command") { mods.metaKey = true; continue; }
        }
        keyToken = token;
      }
      const SPECIAL = {
        enter: { key: "Enter", code: "Enter", keyCode: 13 },
        tab: { key: "Tab", code: "Tab", keyCode: 9 },
        escape: { key: "Escape", code: "Escape", keyCode: 27 },
        esc: { key: "Escape", code: "Escape", keyCode: 27 },
        backspace: { key: "Backspace", code: "Backspace", keyCode: 8 },
        delete: { key: "Delete", code: "Delete", keyCode: 46 },
        space: { key: " ", code: "Space", keyCode: 32 },
        " ": { key: " ", code: "Space", keyCode: 32 },
        arrowup: { key: "ArrowUp", code: "ArrowUp", keyCode: 38 },
        arrowdown: { key: "ArrowDown", code: "ArrowDown", keyCode: 40 },
        arrowleft: { key: "ArrowLeft", code: "ArrowLeft", keyCode: 37 },
        arrowright: { key: "ArrowRight", code: "ArrowRight", keyCode: 39 },
        home: { key: "Home", code: "Home", keyCode: 36 },
        end: { key: "End", code: "End", keyCode: 35 },
        pageup: { key: "PageUp", code: "PageUp", keyCode: 33 },
        pagedown: { key: "PageDown", code: "PageDown", keyCode: 34 },
      };
      const mapped = SPECIAL[keyToken.toLowerCase()];
      if (mapped) return { ...mapped, ...mods };
      if (keyToken.length === 1) {
        const ch = keyToken;
        const upper = ch.toUpperCase();
        let code = ch;
        if (/[A-Z]/.test(upper)) code = `Key${upper}`;
        else if (/[0-9]/.test(ch)) code = `Digit${ch}`;
        return { key: ch, code, keyCode: upper.charCodeAt(0), ...mods };
      }
      return { key: keyToken, code: keyToken, keyCode: 0, ...mods };
    };

    const dispatchKey = (target, spec) => {
      const win = winOf(target);
      const init = {
        key: spec.key,
        code: spec.code,
        keyCode: spec.keyCode,
        which: spec.keyCode,
        ctrlKey: Boolean(spec.ctrlKey),
        altKey: Boolean(spec.altKey),
        shiftKey: Boolean(spec.shiftKey),
        metaKey: Boolean(spec.metaKey),
        bubbles: true,
        cancelable: true,
        composed: true,
      };
      const notCanceled = target.dispatchEvent(new win.KeyboardEvent("keydown", init));
      if (spec.key === "Enter" || spec.key.length === 1) {
        target.dispatchEvent(new win.KeyboardEvent("keypress", init));
      }
      return {
        notCanceled,
        up: () => target.dispatchEvent(new win.KeyboardEvent("keyup", { ...init, cancelable: false })),
      };
    };

    if (action.action === "console") {
      return (window.__crossagentsConsole || []).slice(-150);
    }

    if (action.action === "snapshot") {
      let scopeRoot = document.body || document.documentElement;
      if (action.selector) {
        try {
          scopeRoot = querySelectorDeep(document, action.selector);
        } catch {
          throw new Error(`无效的 CSS 选择器：${action.selector}`);
        }
        if (!scopeRoot) throw new Error(`页面中未找到匹配选择器的区域：${action.selector}`);
      }
      const nodes = [];
      const state = { total: 0 };
      if (scopeRoot && scopeRoot.nodeType === 1 && scopeRoot.matches?.(INTERACTIVE) && isVisible(scopeRoot)) {
        state.total++;
        nodes.push(scopeRoot);
      }
      walkInteractive(scopeRoot, nodes, state);
      window[TAG] = nodes;

      const refs = nodes.map((node, index) => describeNode(node, index));
      if (state.total > nodes.length) {
        refs.push(`... (${state.total - nodes.length} more interactive elements omitted; scroll or pass selector to narrow)`);
      }

      const maxChars = Math.min(32000, Math.max(500, Number(action.max_chars) || 16000));
      const fullText = collectVisibleText(scopeRoot);
      const pageText =
        fullText.length > maxChars
          ? `${fullText.slice(0, maxChars)}\n... [truncated: showing ${maxChars} of ${fullText.length} chars]`
          : fullText;

      const scroller = findPrimaryScroller();
      const header = `Viewport: ${window.innerWidth}x${window.innerHeight} | Scroll: ${scroller.y}/${scroller.max}px`;
      const sections = [header];
      sections.push(refs.length ? `Interactive elements (${nodes.length}):\n${refs.join("\n")}` : "Interactive elements (0): none");
      if (pageText) sections.push(`Page text:\n${pageText}`);
      return { title: document.title, url: location.href, text: sections.join("\n\n") };
    }

    if (action.action === "probe") {
      if (action.selector) {
        try {
          document.querySelector(action.selector);
        } catch {
          throw new Error(`无效的 CSS 选择器：${action.selector}`);
        }
      }
      let matched = true;
      const currentUrl = location.href;
      if (matched && action.url) {
        matched = currentUrl.includes(action.url);
      }
      if (matched && (action.text || action.text_gone)) {
        const text = collectVisibleText(document.body || document.documentElement);
        if (action.text && !text.includes(action.text)) matched = false;
        if (action.text_gone && text.includes(action.text_gone)) matched = false;
      }
      if (matched && action.selector) {
        matched = Boolean(queryVisibleSelectorDeep(document, action.selector));
      }
      return { matched, url: currentUrl, title: document.title, readyState: document.readyState };
    }

    if (action.action === "scroll") {
      const dy = Number(action.dy) || 0;
      if (action.reference != null) {
        const node = window[TAG]?.[action.reference - 1];
        if (!node?.isConnected) throw new Error("页面已变化，请重新获取页面快照");
        let target = node;
        while (target && !isScrollable(target)) {
          target = target.parentElement || target.getRootNode?.()?.host || null;
        }
        if (target) {
          target.scrollBy({ top: dy, behavior: "instant" });
          const max = Math.max(0, Math.round(target.scrollHeight - target.clientHeight));
          const y = Math.round(target.scrollTop);
          return { ok: true, scroll_y: y, max_scroll_y: max, at_top: y <= 1, at_bottom: y >= max - 1 };
        }
      }
      const beforeWin = window.scrollY;
      window.scrollBy({ top: dy, behavior: "instant" });
      if (Math.abs(window.scrollY - beforeWin) >= 1 || Math.abs(dy) < 1) {
        const se = document.scrollingElement || document.documentElement;
        const max = Math.max(0, Math.round((se?.scrollHeight || 0) - window.innerHeight));
        const y = Math.round(window.scrollY);
        return { ok: true, scroll_y: y, max_scroll_y: max, at_top: y <= 1, at_bottom: y >= max - 1 };
      }
      const primary = findPrimaryScroller();
      if (primary.el) {
        primary.el.scrollBy({ top: dy, behavior: "instant" });
        const max = Math.max(0, Math.round(primary.el.scrollHeight - primary.el.clientHeight));
        const y = Math.round(primary.el.scrollTop);
        return { ok: true, scroll_y: y, max_scroll_y: max, at_top: y <= 1, at_bottom: y >= max - 1 };
      }
      return { ok: true, scroll_y: primary.y, max_scroll_y: primary.max, at_top: primary.y <= 1, at_bottom: primary.y >= primary.max - 1 };
    }

    if (action.action === "press") {
      const target = deepActiveElement();
      const spec = parseKeyChord(action.key);
      const { notCanceled, up } = dispatchKey(target, spec);
      const noMods = !spec.ctrlKey && !spec.altKey && !spec.shiftKey && !spec.metaKey;
      if (notCanceled && target) {
        const win = winOf(target);
        if (spec.key === "Enter" && noMods) {
          if (target.matches?.("button, a[href], summary, [role=button], [role=link]")) {
            target.click?.();
          } else if (target instanceof win.HTMLInputElement && target.form) {
            try { target.form.requestSubmit(); } catch { target.form.submit(); }
          }
        } else if (spec.key === " " && noMods && !isTextControl(target)) {
          if (target.matches?.("button, input[type=checkbox], input[type=radio], [role=button], [role=checkbox], [role=switch]")) {
            target.click?.();
          }
        } else if (spec.key === "Tab" && !spec.ctrlKey && !spec.altKey && !spec.metaKey) {
          const focusables = [];
          walkInteractive(document.body || document.documentElement, focusables, { total: 0 });
          const enabled = focusables.filter((el) => !isDisabled(el) && el.tabIndex >= 0);
          if (enabled.length) {
            const idx = enabled.indexOf(target);
            const next = spec.shiftKey
              ? enabled[(idx <= 0 ? enabled.length : idx) - 1]
              : enabled[(idx + 1) % enabled.length];
            next?.focus?.();
          }
        } else if ((spec.ctrlKey || spec.metaKey) && !spec.altKey && spec.key.toLowerCase() === "a") {
          if (target instanceof win.HTMLInputElement || target instanceof win.HTMLTextAreaElement) {
            target.select();
          }
        } else if (noMods && !isTextControl(target) && ["PageDown", "PageUp", "Home", "End"].includes(spec.key)) {
          const scroller = findPrimaryScroller();
          const step = Math.round(window.innerHeight * 0.85);
          const scrollTarget = scroller.el || window;
          if (spec.key === "PageDown") scrollTarget.scrollBy({ top: step, behavior: "instant" });
          else if (spec.key === "PageUp") scrollTarget.scrollBy({ top: -step, behavior: "instant" });
          else if (spec.key === "Home") scrollTarget.scrollTo({ top: 0, behavior: "instant" });
          else if (spec.key === "End") scrollTarget.scrollTo({ top: scroller.max, behavior: "instant" });
        }
      }
      up();
      return { ok: true };
    }

    let node = window[TAG]?.[action.reference - 1];
    if (!node?.isConnected) throw new Error("页面已变化，请重新获取页面快照");

    if (action.action === "click") {
      if (isDisabled(node)) throw new Error("该元素当前处于禁用状态（disabled）");
      node.scrollIntoView({ block: "center", inline: "nearest" });
      const rect = node.getBoundingClientRect();
      const clientX = Math.round(rect.left + rect.width / 2);
      const clientY = Math.round(rect.top + rect.height / 2);
      const win = winOf(node);
      const mouseInit = { bubbles: true, cancelable: true, composed: true, view: win, button: 0, buttons: 1, clientX, clientY };
      const pointerInit = { ...mouseInit, pointerId: 1, pointerType: "mouse", isPrimary: true };
      if (typeof win.PointerEvent === "function") {
        node.dispatchEvent(new win.PointerEvent("pointerover", pointerInit));
        node.dispatchEvent(new win.PointerEvent("pointerdown", pointerInit));
      }
      node.dispatchEvent(new win.MouseEvent("mouseover", mouseInit));
      node.dispatchEvent(new win.MouseEvent("mousedown", mouseInit));
      if (typeof node.focus === "function" && (node.tabIndex >= 0 || node.matches?.("a[href], button, input, textarea, select, [contenteditable]"))) {
        try { node.focus({ preventScroll: true }); } catch { node.focus(); }
      }
      if (typeof win.PointerEvent === "function") {
        node.dispatchEvent(new win.PointerEvent("pointerup", { ...pointerInit, buttons: 0 }));
      }
      node.dispatchEvent(new win.MouseEvent("mouseup", { ...mouseInit, buttons: 0 }));
      if (typeof node.click === "function") {
        node.click();
      } else {
        node.dispatchEvent(new win.MouseEvent("click", { ...mouseInit, buttons: 0 }));
      }
      return { ok: true };
    }

    if (action.action === "select") {
      if (node.tagName === "LABEL" && node.control) node = node.control;
      const win = winOf(node);
      if (!(node instanceof win.HTMLSelectElement)) {
        throw new Error("该元素不是下拉选择框（<select>）");
      }
      if (isDisabled(node)) throw new Error("该下拉选择框当前处于禁用状态（disabled）");
      const wanted = String(action.value ?? "");
      const wantedTrim = wanted.trim();
      const wantedLower = wantedTrim.toLowerCase();
      const options = Array.from(node.options || []).filter((o) => !o.disabled);
      const match =
        options.find((o) => o.value === wanted) ||
        options.find((o) => clean(o.textContent, 200) === wantedTrim) ||
        options.find((o) => o.value.toLowerCase() === wantedLower) ||
        options.find((o) => clean(o.textContent, 200).toLowerCase() === wantedLower);
      if (!match) {
        const preview = options.slice(0, 15).map((o) => {
          const l = clean(o.textContent, 40);
          return l && o.value && l !== o.value ? `${l} (${o.value})` : (l || o.value);
        });
        throw new Error(`未找到匹配选项 ${JSON.stringify(wanted)}；可选值：${preview.join("、") || "无"}`);
      }
      node.scrollIntoView({ block: "center", inline: "nearest" });
      node.focus();
      const setter = Object.getOwnPropertyDescriptor(win.HTMLSelectElement.prototype, "value")?.set;
      if (setter) setter.call(node, match.value);
      else node.value = match.value;
      match.selected = true;
      node.dispatchEvent(new win.Event("input", { bubbles: true, composed: true }));
      node.dispatchEvent(new win.Event("change", { bubbles: true }));
      return {
        ok: true,
        selected_value: match.value,
        selected_label: clean(match.textContent || match.value, 120),
      };
    }

    if (action.action === "type") {
      if (node.tagName === "LABEL" && node.control) node = node.control;
      const win = winOf(node);
      if (node instanceof win.HTMLSelectElement) {
        throw new Error("该元素是下拉选择框，请使用 browser_select 选择选项");
      }
      if (!(node instanceof win.HTMLInputElement || node instanceof win.HTMLTextAreaElement || node.isContentEditable)) {
        const inner = node.querySelector?.("input:not([type=hidden]), textarea, [contenteditable=''], [contenteditable='true'], [contenteditable='plaintext-only']");
        if (inner) node = inner;
      }
      if (isDisabled(node)) throw new Error("该输入框当前处于禁用状态（disabled）");
      if (node.readOnly || node.getAttribute?.("aria-readonly") === "true") {
        throw new Error("该输入框是只读的（readonly）");
      }
      node.scrollIntoView({ block: "center", inline: "nearest" });
      node.focus();
      const text = String(action.text ?? "");
      const clear = action.clear !== false;
      const doc = node.ownerDocument || document;
      if (node instanceof win.HTMLInputElement || node instanceof win.HTMLTextAreaElement) {
        const proto = node instanceof win.HTMLInputElement ? win.HTMLInputElement.prototype : win.HTMLTextAreaElement.prototype;
        const nextValue = clear ? text : `${node.value}${text}`;
        Object.getOwnPropertyDescriptor(proto, "value")?.set?.call(node, nextValue);
        node.dispatchEvent(new win.InputEvent("input", { bubbles: true, composed: true, inputType: "insertText", data: text }));
        node.dispatchEvent(new win.Event("change", { bubbles: true }));
      } else if (node.isContentEditable) {
        const sel = win.getSelection?.();
        if (sel) {
          const range = doc.createRange();
          range.selectNodeContents(node);
          if (!clear) range.collapse(false);
          sel.removeAllRanges();
          sel.addRange(range);
        }
        const before = node.textContent || "";
        node.dispatchEvent(new win.InputEvent("beforeinput", { bubbles: true, cancelable: true, composed: true, inputType: "insertText", data: text }));
        let inserted = false;
        try {
          if (typeof doc.execCommand === "function") {
            inserted = doc.execCommand("insertText", false, text);
          }
        } catch {}
        const expected = clear ? text : `${before}${text}`;
        if (!inserted || (node.textContent || "") !== expected) {
          node.textContent = expected;
          node.dispatchEvent(new win.InputEvent("input", { bubbles: true, composed: true, inputType: "insertText", data: text }));
        }
      } else {
        throw new Error("该元素不是文本输入框");
      }
      if (action.submit) {
        const enter = dispatchKey(node, { key: "Enter", code: "Enter", keyCode: 13 });
        if (enter.notCanceled && node.form) {
          try { node.form.requestSubmit(); } catch { node.form.submit(); }
        }
        enter.up();
      }
      return { ok: true };
    }

    throw new Error(`不支持的浏览器操作：${String(action.action || "")}`);
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) };
  }
}
