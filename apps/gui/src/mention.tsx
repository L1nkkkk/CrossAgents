// `@` in a composer: name who should help, a teammate (`@Iris`) or a model to recruit an agent
// on (`@claude-haiku-4-5`). Typing `@` opens a menu above the composer; arrows move, Enter or
// Tab picks, Escape closes. The pick is plain text in the message; the agent's guide says what
// `@` means (crates/orchestrator/src/protocol.rs).

import { useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import type { Agent } from "@bindings/Agent";
import type { PluginInfo } from "@bindings/PluginInfo";
import { Avatar, VendorMark, agentTitle, displayName } from "./components";
import { prettyModel, vendorLabel } from "./lib/models";

export type Mention = { key: string; token: string; search: string; label: ReactNode; sub?: ReactNode; icon?: ReactNode; group: string };

/** Teammates first (by name), then every model a login offers (by id). */
export function mentionsFor(agents: Agent[], plugins: PluginInfo[]): Mention[] {
  const mates = agents.map(
    (a): Mention => ({
      key: `a/${a.id}`,
      token: a.name,
      search: `${a.name} ${displayName(a)} ${a.model ?? ""}`.toLowerCase(),
      label: displayName(a),
      sub: `${agentTitle(a)} · ${a.model ? prettyModel(a.model) : a.plugin}`,
      icon: <Avatar agent={a} size={16} />,
      group: "协作者",
    }),
  );
  const seen = new Set<string>();
  const models = plugins.flatMap((p) =>
    p.models
      .filter((m) => !seen.has(m) && !!seen.add(m))
      .map(
        (m): Mention => ({
          key: `m/${p.name}/${m}`,
          token: m,
          search: `${m} ${prettyModel(m)} ${vendorLabel(p.name)}`.toLowerCase(),
          label: `${vendorLabel(p.name)} ${prettyModel(m)}`,
          sub: m,
          icon: <VendorMark plugin={p.name} size={14} />,
          group: "找一个模型来做",
        }),
      ),
  );
  return [...mates, ...models];
}

/** Rows the menu shows at most. */
const SHOWN = 10;

/**
 * Wires `@` into a textarea: call `sync` from its onChange and onClick, give `onKeyDown` the
 * first look at keys (true: handled), and render `menu` inside the composer box.
 */
export function useMention(ref: RefObject<HTMLTextAreaElement | null>, setValue: (v: string) => void, candidates: Mention[]) {
  const [at, setAt] = useState<{ start: number; query: string }>();
  const [cursor, setCursor] = useState(0);

  const sync = () => {
    const el = ref.current;
    if (!el) return;
    const pos = el.selectionStart ?? el.value.length;
    const m = /(^|\s)@([^\s@]*)$/.exec(el.value.slice(0, pos));
    if (!m) return setAt(undefined);
    const start = pos - m[2].length - 1;
    if (at?.start !== start || at.query !== m[2]) setCursor(0);
    setAt({ start, query: m[2] });
  };

  const q = at?.query.toLowerCase() ?? "";
  const shown = at ? candidates.filter((c) => !q || c.search.includes(q) || c.token.toLowerCase().includes(q)).slice(0, SHOWN) : [];
  const open = !!at && shown.length > 0;

  const pick = (c: Mention) => {
    const el = ref.current;
    if (!el || !at) return;
    const end = at.start + 1 + at.query.length;
    const before = el.value.slice(0, at.start) + "@" + c.token + " ";
    const next = before + el.value.slice(end).replace(/^ /, "");
    setValue(next);
    setAt(undefined);
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(before.length, before.length);
    });
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>): boolean => {
    if (!open || e.nativeEvent.isComposing) return false;
    if (e.key === "ArrowDown") setCursor((c) => (c + 1) % shown.length);
    else if (e.key === "ArrowUp") setCursor((c) => (c - 1 + shown.length) % shown.length);
    else if ((e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey) || e.key === "Tab") pick(shown[Math.min(cursor, shown.length - 1)]);
    else if (e.key === "Escape") setAt(undefined);
    else return false;
    e.preventDefault();
    e.stopPropagation();
    return true;
  };

  const menu = open ? (
    <div className="menu up left mention-menu" role="listbox" aria-label="点名协作者或模型">
      {shown.map((c, i) => (
        <div key={c.key} style={{ display: "contents" }}>
          {(i === 0 || shown[i - 1].group !== c.group) && <div className="menu-head">{c.group}</div>}
          <button
            type="button"
            role="option"
            aria-selected={i === cursor}
            className={`menu-item ${i === cursor ? "cursor" : ""}`}
            onMouseEnter={() => setCursor(i)}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => pick(c)}
          >
            {c.icon && <span className="menu-icon">{c.icon}</span>}
            <span className="menu-text">
              <span className="menu-label">{c.label}</span>
              {c.sub && <span className="menu-sub">{c.sub}</span>}
            </span>
          </button>
        </div>
      ))}
    </div>
  ) : null;

  return { sync, onKeyDown, menu };
}
