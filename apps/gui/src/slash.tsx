// `/` at the very start of a composer: the commands offered above it, filtered as you type
// (matching: lib/slash.ts). Arrows move, Enter or Tab runs, Escape closes — the same keys and
// the same look as `@` (src/mention.tsx). CrossAgents' own commands come first; the second
// group is whatever the agent's CLI brings, which the composer can pass in.

import { useEffect, useState, type KeyboardEvent, type RefObject } from "react";
import { matchCommands, type SlashCommand, type SlashGroup } from "./lib/slash";

/**
 * A command the menu can offer. With `run` it happens on pick (and the input clears); without
 * one, picking writes `/${name} ` into the input so arguments can follow — what the agent's own
 * commands need.
 */
export type SlashEntry = SlashCommand & { run?: () => void; kind?: "command" | "skill" };

export type SlashMenuGroup = Omit<SlashGroup, "commands"> & { commands: SlashEntry[]; note?: string };


export const SLASH_MENU_ID = "composer-slash-menu";

/**
 * Wires `/` into a textarea: call `sync` from its onChange and onClick, give `onKeyDown` the
 * first look at keys (true: handled), spread `textarea` onto it for the combobox aria, and
 * render `menu` inside the composer box.
 */
export function useSlash(ref: RefObject<HTMLTextAreaElement | null>, setValue: (v: string) => void, groups: SlashMenuGroup[]) {
  // The text after the slash while the menu is open; undefined: closed.
  const [query, setQuery] = useState<string>();
  const [cursor, setCursor] = useState(0);

  const sync = () => {
    const el = ref.current;
    if (!el) return;
    // Only a message that is a lone command: a slash mid-sentence is just text.
    const m = /^\/(\S*)$/.exec(el.value);
    if (!m) return setQuery(undefined);
    if (m[1] !== query) setCursor(0);
    setQuery(m[1]);
  };

  const shown = query === undefined ? [] : groups.flatMap((g) => matchCommands([g], query) as SlashEntry[]);
  const open = query !== undefined && (shown.length > 0 || groups.some((g) => g.note));
  const at = Math.min(cursor, Math.max(0, shown.length - 1));
  const label = (key: string) => groups.find((g) => g.key === key)?.label ?? key;
  const idOf = (c: SlashCommand) => `${SLASH_MENU_ID}-${c.key.replace(/[^\w-]/g, "-")}`;

  useEffect(() => {
    if (open && shown[at]) document.getElementById(idOf(shown[at]))?.scrollIntoView({ block: "nearest" });
  }, [open, at, query]);

  const pick = (c: SlashEntry) => {
    // A blocked command says why in its row; picking it does nothing rather than send it.
    if (c.blocked) return;
    const el = ref.current;
    setQuery(undefined);
    if (c.run) {
      setValue("");
      c.run();
      requestAnimationFrame(() => el?.focus());
      return;
    }
    const next = `/${c.name} `;
    setValue(next);
    requestAnimationFrame(() => {
      el?.focus();
      el?.setSelectionRange(next.length, next.length);
    });
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>): boolean => {
    if (!open || e.nativeEvent.isComposing) return false;
    if (!shown.length) {
      if (e.key !== "Escape") return false;
      setQuery(undefined);
      e.preventDefault();
      return true;
    }
    if (e.key === "ArrowDown") setCursor((at + 1) % shown.length);
    else if (e.key === "ArrowUp") setCursor((at - 1 + shown.length) % shown.length);
    else if ((e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey) || e.key === "Tab") pick(shown[at]);
    else if (e.key === "Escape") setQuery(undefined);
    else return false;
    e.preventDefault();
    e.stopPropagation();
    return true;
  };

  const menu = open ? (
    <div className="menu up left mention-menu slash-menu" role="listbox" id={SLASH_MENU_ID} aria-label="指令">
      {shown.map((c, i) => (
        <div key={c.key} style={{ display: "contents" }}>
          {(i === 0 || shown[i - 1].group !== c.group) && <div className="menu-head">{label(c.group)}</div>}
          <button
            type="button"
            role="option"
            id={idOf(c)}
            aria-selected={i === at}
            aria-disabled={c.blocked ? true : undefined}
            className={`menu-item ${i === at ? "cursor" : ""} ${c.blocked ? "blocked" : ""}`}
            onMouseEnter={() => setCursor(i)}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => pick(c)}
          >
            <span className="menu-text">
              <span className="menu-label">
                <code className="slash-name">/{c.name}</code>
                {c.kind && <span className="slash-kind">{c.kind === "skill" ? "skill" : "指令"}</span>}
                {!c.kind && <span>{c.title}</span>}
              </span>
              {(c.blocked || c.sub || (c.kind && c.title)) && <span className="menu-sub">{c.blocked ? `不可用 · ${c.blocked}` : c.sub ?? c.title}</span>}
            </span>
          </button>
        </div>
      ))}
      {groups.filter((g) => g.note).map((g) => <div key={`${g.key}-note`}><div className="menu-head">{g.label}</div><div className="slash-note">{g.note}</div></div>)}
    </div>
  ) : null;

  // combobox on the textarea, listbox above it: the active row is named, not focused.
  const textarea = {
    role: "combobox",
    "aria-expanded": open,
    "aria-controls": open ? SLASH_MENU_ID : undefined,
    "aria-activedescendant": open && shown.length ? idOf(shown[at]) : undefined,
    "aria-autocomplete": "list" as const,
  };

  return { sync, onKeyDown, menu, textarea };
}
