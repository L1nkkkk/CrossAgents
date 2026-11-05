// 活动: every agent's replies, tool calls and messages on one timeline (docs/TODO.md §二十五
// 第 7 条). The events are the hub's own stream; this page keeps what it saw since it loaded,
// and the note at the top says so. Entries are buttons: one opens the agent's conversation and
// scrolls to the turn it belongs to (the transcript's `[data-turn]`), or says why it cannot.

import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { displayName, Icon, sessionHref, useAgents, type IconName } from "./components";
import { onHubEvent, type HubEvent } from "./hub";
import { buildFeed, descendantsOf, feedSummary, FEED_TYPES, filterFeed, type FeedEntry, type FeedScope, type FeedType } from "./lib/activityFeed";
import { clock, full } from "./lib/time";
import "./activityfeed.css";

const KEEP = 1500;
const TYPE_LABEL: Record<FeedType, string> = { error: "错误", reply: "回复", tool: "工具", message: "消息" };
const TYPE_ICON: Record<FeedType, IconName> = { error: "info", reply: "chat", tool: "terminal", message: "send" };

/**
 * What this page has seen since it loaded. Kept here rather than in a hook so a tab opened later
 * still shows the events that arrived before it (the module is loaded with the app).
 */
let events: HubEvent[] = [];
let revision = 0;
let started = false;
const listeners = new Set<() => void>();

function startFeed() {
  if (started) return;
  started = true;
  onHubEvent((event) => {
    events.push(event);
    if (events.length > KEEP) events.splice(0, events.length - KEEP);
    revision += 1;
    listeners.forEach((listener) => listener());
  });
}
startFeed();

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export function ActivityFeed() {
  const version = useSyncExternalStore(subscribe, () => revision);
  const agents = useAgents();
  const [types, setTypes] = useState<Set<FeedType>>(() => new Set(FEED_TYPES));
  const [scope, setScope] = useState("all");
  const [withSubs, setWithSubs] = useState(false);
  const [notice, setNotice] = useState<string>();
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);

  const feed = useMemo(() => buildFeed(events), [version]);
  const names = useMemo(() => new Map((agents.data ?? []).map((agent) => [agent.id, agent.name] as const)), [agents.data]);
  const nameOf = (id: string) => names.get(id);
  const picked: FeedScope = scope === "all" ? { kind: "all" } : { kind: withSubs ? "team" : "agent", agent: scope };
  const shown = useMemo(
    () => filterFeed(feed, { types, scope: picked, agents: agents.data ?? [] }),
    // `picked` is rebuilt each render; its fields are what matter.
    [feed, types, scope, withSubs, agents.data],
  );
  const under = scope !== "all" && agents.data ? descendantsOf(agents.data, scope).size - 1 : 0;

  useEffect(() => () => clearTimeout(timer.current), []);

  const toggleType = (type: FeedType) => {
    setTypes((old) => {
      const next = new Set(old);
      if (next.has(type)) next.delete(type);
      else next.add(type);
      return next;
    });
  };

  const reset = () => {
    setTypes(new Set(FEED_TYPES));
    setScope("all");
    setWithSubs(false);
  };

  /** Opens the agent's conversation, then scrolls to the turn if the transcript has it yet. */
  const jump = (entry: FeedEntry) => {
    const agent = agents.data?.find((a) => a.id === entry.agentId);
    if (!agent) {
      setNotice("找不到这个 agent 的会话，它可能已经被移除。");
      return;
    }
    clearTimeout(timer.current);
    setNotice(undefined);
    window.location.hash = sessionHref(agent);
    if (!entry.turn) {
      setNotice(`已打开 ${displayName(agent)} 的会话；这一条没有可定位的轮次，所以只打开会话。`);
      return;
    }
    const until = Date.now() + 4000;
    const seek = () => {
      const element = document.querySelector<HTMLElement>(`[data-turn="${CSS.escape(entry.turn!)}"]`);
      if (element) {
        element.scrollIntoView({ block: "center", behavior: "smooth" });
        setNotice(undefined);
        return;
      }
      if (Date.now() < until) {
        timer.current = setTimeout(seek, 100);
        return;
      }
      setNotice(`已打开 ${displayName(agent)} 的会话，但没找到这一条的位置（可能来自更早的会话）。`);
    };
    timer.current = setTimeout(seek, 60);
  };

  const whyEmpty = (
    <p className="muted small af-empty">
      打开页面以来还没有活动。hub 只推送页面加载之后的事件，所以更早的轮次不在这里；请到各自的会话里查看。
    </p>
  );

  return (
    <div className="activity-feed">
      <div className="af-filters" role="group" aria-label="活动类型筛选">
        {FEED_TYPES.map((type) => {
          const on = types.has(type);
          return (
            <button
              key={type}
              type="button"
              className={`af-type af-${type} ${on ? "on" : ""}`}
              aria-pressed={on}
              aria-label={`${on ? "隐藏" : "显示"}${TYPE_LABEL[type]}活动`}
              title={`${on ? "隐藏" : "显示"}${TYPE_LABEL[type]}活动`}
              onClick={() => toggleType(type)}
            >
              <Icon name={TYPE_ICON[type]} size={13} />
              {TYPE_LABEL[type]}
            </button>
          );
        })}
      </div>
      <div className="af-scope">
        <label className="af-field">
          <span className="af-label">范围</span>
          <select aria-label="活动范围" value={scope} onChange={(e) => setScope(e.target.value)}>
            <option value="all">全部 agent</option>
            {(agents.data ?? []).map((agent) => (
              <option key={agent.id} value={agent.id}>
                {displayName(agent)}
              </option>
            ))}
          </select>
        </label>
        {scope !== "all" && (
          <label className="af-subs" title="也显示它派出的 worker、spawn 的 agent 以及它们的下属">
            <input type="checkbox" checked={withSubs} onChange={(e) => setWithSubs(e.target.checked)} />
            含下属{under > 0 ? `（${under}）` : ""}
          </label>
        )}
      </div>
      <p className="af-note" title="活动来自 hub 的事件流；本页只收到页面加载后 hub 推来的事件，最多保留最近 1500 条。">
        只显示打开页面后的活动 · {feed.length} 条 · 最新在上
      </p>
      {notice && (
        <p className="af-notice" role="status">
          {notice}
        </p>
      )}
      {feed.length === 0 ? (
        whyEmpty
      ) : shown.length === 0 ? (
        <p className="muted small af-empty">
          没有符合筛选的活动。
          <button type="button" className="ghost small" onClick={reset}>
            显示全部
          </button>
        </p>
      ) : (
        <ol className="af-list">
          {shown.map((entry) => {
            const name = nameOf(entry.agentId) ?? "未知 agent";
            const summary = feedSummary(entry, nameOf);
            return (
              <li key={entry.id}>
                <button
                  type="button"
                  className={`af-row af-${entry.type} ${entry.failed ? "af-failed" : ""}`}
                  onClick={() => jump(entry)}
                  title={`${full(entry.at)} · ${summary}\n点击打开 ${name} 的会话并定位这一条`}
                  aria-label={`${TYPE_LABEL[entry.type]}，${name}，${clock(entry.at)}，${summary}。打开会话中的这一条`}
                >
                  <time className="af-time" dateTime={entry.at}>
                    {clock(entry.at)}
                  </time>
                  <span className="af-agent">{name}</span>
                  <span className="af-icon" aria-hidden="true">
                    <Icon name={TYPE_ICON[entry.type]} size={13} />
                  </span>
                  <span className="af-sum">{summary}</span>
                </button>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}
