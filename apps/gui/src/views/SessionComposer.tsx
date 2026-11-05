import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { Capabilities } from "@bindings/Capabilities";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import { api, errorText } from "../api";
import { HeldFiles, MiniFiles, useHeld } from "../attach";
import { ErrorBox, Icon, Vendor, displayName } from "../components";
import { useCompact } from "../contextusage";
import { useHubData } from "../hub";
import { supportNote } from "../lib/attachments";
// `draft` is this file's word for the text being typed, so the module keeps the plural.
import * as drafts from "../lib/draft";
import { t } from "../lib/i18n";
import { canSteer, composerAction, composerHint, composerPlaceholder } from "../lib/steer";
import { mentionsFor, useMention } from "../mention";
import { ModelEffortPicks } from "../modelPick";
import { sendMessage } from "../outbox";
import { StateWord } from "../parts";
import type { TranscriptTurn } from "../session";
import { useSlash, type SlashMenuGroup } from "../slash";
import { PlanTasks, usePlanApproval } from "./Run";
import { isKickoff } from "../lib/messageText";
import { fromPerson } from "./SessionMessage";

const QUEUE_OPEN = "crossagents.queueOpen";

/**
 * "接下来": messages waiting for their turn, on the composer's top edge. Your own can be sent
 * now (interrupting the current turn), edited (taken back into the composer) or removed;
 * orchestration messages are shown but left alone.
 */
export function QueuePanel({ agent, queued, onEdit }: { agent: Agent; queued: TranscriptTurn[]; onEdit: (text: string) => void }) {
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem(QUEUE_OPEN) !== "0";
    } catch {
      return true;
    }
  });
  const [error, setError] = useState<string>();
  if (!queued.length) return null;

  const toggle = () => {
    setOpen(!open);
    try {
      localStorage.setItem(QUEUE_OPEN, open ? "0" : "1");
    } catch {
      // Remembered for this page only.
    }
  };
  const act = async (action: () => Promise<unknown>) => {
    setError(undefined);
    try {
      await action();
    } catch (err) {
      setError(errorText(err));
    }
  };
  const withdraw = (turn: TranscriptTurn) => api.withdrawMessage(agent.id, turn.message_id);
  // A queued message's files, back in the shape the send API takes: "send now" re-sends it
  // as a whole, text and files, so an image-only message does not go out empty.
  const uploads = (turn: TranscriptTurn) =>
    Promise.all((turn.attachments ?? []).map(async (f) => ({ name: f.name, mime: f.mime, data: (await api.attachment(agent.id, f.id)).data })));

  return (
    <div className="queue">
      <button className="queue-head" onClick={toggle} aria-expanded={open}>
        <span className="disclosure" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
        接下来 · {queued.length}
        <span className="muted small">本轮结束后依次发给 {displayName(agent)}</span>
      </button>
      {open &&
        queued.map((turn) => {
          const mine = fromPerson(turn);
          return (
            <div key={turn.message_id} className="queue-row">
              <span className="queue-kind" title={mine ? "你发的" : "编排"}>
                {mine ? "你" : "编排"}
              </span>
              {!!turn.attachments?.length && <MiniFiles agent={agent.id} files={turn.attachments} />}
              {turn.input && <span className="queue-text">{!mine && isKickoff(turn.input) ? "开始新一轮" : turn.input.split("\n")[0]}</span>}
              {mine && (
                <span className="queue-actions">
                  <button
                    className="link"
                    title="打断当前这一轮，先发这条"
                    onClick={() =>
                      act(async () => {
                        const attachments = await uploads(turn);
                        await withdraw(turn);
                        await api.sendMessage(agent.id, { text: turn.input, now: true, ...(attachments.length ? { attachments } : {}) });
                      })
                    }
                  >
                    立即发送
                  </button>
                  {!turn.attachments?.length && (
                    <button
                      className="link"
                      onClick={() =>
                        act(async () => {
                          await withdraw(turn);
                          onEdit(turn.input);
                        })
                      }
                    >
                      编辑
                    </button>
                  )}
                  <button className="link" onClick={() => act(() => withdraw(turn))}>
                    删除
                  </button>
                </span>
              )}
            </div>
          );
        })}
      {error && <ErrorBox error={error} />}
    </div>
  );
}

export function Composer({
  agent,
  capabilities,
  driver,
  draft,
  setDraft,
  plans,
  onPlanDone,
  mates,
}: {
  agent: Agent;
  capabilities: Capabilities | null;
  driver: string | null;
  draft: string;
  setDraft: (text: string) => void;
  plans: RunDetail[];
  onPlanDone: () => void;
  /** Agents @ offers, besides the models. */
  mates: Agent[];
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [deferred, setDeferred] = useState<string[]>([]);
  const [keepNote, setKeepNote] = useState<string>();
  const ref = useRef<HTMLTextAreaElement>(null);
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const mention = useMention(ref, setDraft, mentionsFor(mates, (plugins.data ?? []).filter((p) => p.models.length > 0)));
  const compact = useCompact(agent);
  const agentCommands = useHubData(() => api.agentCommands(agent.id), [agent.id],
    (e) => (e.type === "agent.status_changed" && e.data.id === agent.id) || (e.type === "agent.updated" && e.data.agent.id === agent.id));
  const groups: SlashMenuGroup[] = [
    {
      key: "crossagents",
      label: "CrossAgents 指令",
      commands: [
        {
          key: "compact",
          group: "crossagents",
          name: "compact",
          title: "压缩上下文",
          sub: "把历史压成摘要，腾出上下文",
          blocked: compact.blocked,
          run: () => void compact.compact(),
        },
      ],
    },
    {
      key: "agent",
      label: "agent 自带",
      note: agentCommands.error ? `获取失败：${agentCommands.error}` : agentCommands.data && !agentCommands.data.known ? (agentCommands.data.note ?? "这个驱动不报告自带指令") : undefined,
      commands: agentCommands.error || !agentCommands.data?.known ? [] : agentCommands.data.commands
        .filter((c) => c.name.toLowerCase() !== "compact")
        .map((c) => ({ key: `agent/${c.name}`, group: "agent", name: c.name, title: c.description ?? "", kind: c.kind })),
    },
  ];
  const slash = useSlash(ref, setDraft, groups);
  const running = agent.status === "running";
  const canInterrupt = running && capabilities?.interrupt !== false;
  const steer = canSteer(capabilities);
  const plan = plans.find((d) => !deferred.includes(d.run.id));
  const support = capabilities?.attachments ?? null;
  const files = useHeld(setError, support, driver);
  const note = supportNote(support, driver);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 240)}px`;
  }, [draft, plan]);

  useEffect(() => {
    if (!plan) ref.current?.focus();
  }, [agent.id, plan]);

  // Leaving the conversation unmounts the composer, so what was typed and attached is kept
  // under this agent's own key and comes back when you return (lib/draft.ts). Restored once
  // per conversation: a later render must not overwrite what is being typed now.
  const restored = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (restored.current === agent.id) return;
    restored.current = agent.id;
    const kept = drafts.read({ kind: "session", agent: agent.id });
    if (!kept) return;
    setDraft(kept.text);
    if (kept.held.length) files.restore(kept.held);
  }, [agent.id]);

  useEffect(() => {
    if (restored.current !== agent.id) return;
    const timer = setTimeout(() => {
      const out = drafts.write({ kind: "session", agent: agent.id }, { text: draft, title: "", held: files.held });
      setKeepNote(!out.kept && files.held.length ? t("draft.notKept") : out.dropped ? t("draft.filesTooBig", { n: out.dropped }) : undefined);
    }, 400);
    return () => clearTimeout(timer);
  }, [draft, files.held, agent.id]);

  /** `queue`: behind the running turn. `now`: interrupt it and send this. `steer`: into it. */
  async function send(mode: "queue" | "now" | "steer" = "queue") {
    const text = draft.trim();
    const held = files.held;
    if ((!text && !held.length) || busy) return;
    setBusy(true);
    setError(undefined);
    // Shown at once (outbox.ts); a failure puts the text and files back here.
    setDraft("");
    files.clear();
    drafts.forget({ kind: "session", agent: agent.id });
    setKeepNote(undefined);
    try {
      const attachments = held.map(({ name, mime, data }) => ({ name, mime, data }));
      await sendMessage(agent.id, text, { now: mode === "now", steer: mode === "steer", takeBack: true, attachments });
    } catch (err) {
      setDraft(text);
      files.restore(held);
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  async function interrupt() {
    try {
      await api.cancelAgent(agent.id);
    } catch (err) {
      setError(errorText(err));
    }
  }

  function onKey(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.nativeEvent.isComposing) return;
    if (e.key === "Escape") {
      if (canInterrupt) {
        e.preventDefault();
        void interrupt();
      }
      return;
    }
    const action = composerAction({ key: e.key, ctrl: e.ctrlKey || e.metaKey, shift: e.shiftKey, running, steer });
    if (!action) return;
    e.preventDefault();
    // A driver that cannot steer answers Ctrl+Enter with "queue" (lib/steer.ts).
    void send(action === "interrupt-send" ? "now" : action === "steer" ? "steer" : "queue");
  }

  if (plan) {
    return <PlanDock key={plan.run.id} run={plan.run} onDone={onPlanDone} onReply={() => setDeferred([...deferred, plan.run.id])} />;
  }

  return (
    <div className="composer-wrap">
      {error && <ErrorBox error={error} />}
      {compact.error && <ErrorBox error={compact.error} />}
      {plans.length > 0 && (
        <button className="dock-recall" onClick={() => setDeferred([])}>
          <span className="disclosure" aria-hidden="true">
            ▴
          </span>
          Plan 待审批 · {plans[0].run.plan.length} 个任务 · 展开审批
        </button>
      )}
      <div className="composer" onDrop={files.onDrop} onDragOver={files.onDragOver}>
        {slash.menu}
        {mention.menu}
        <HeldFiles files={files.held} onRemove={files.remove} />
        {keepNote && <div className="small dim">{keepNote}</div>}
        <textarea
          {...slash.textarea}
          onPaste={files.onPaste}
          ref={ref}
          rows={1}
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            slash.sync();
            mention.sync();
          }}
          onClick={() => {
            slash.sync();
            mention.sync();
          }}
          onKeyDown={(e) => slash.onKeyDown(e) || mention.onKeyDown(e) || onKey(e)}
          placeholder={composerPlaceholder({ name: displayName(agent), running, steer })}
        />
        <div className="bar">
          {agent.plugin === "external" ? (
            <span className="chip">
              <Vendor plugin={agent.plugin} model={agent.model} />
            </span>
          ) : (
            <ModelEffortPicks agent={agent} />
          )}
          {capabilities?.resume && <span className="chip" title="会话可挂起后恢复">resumable</span>}
          <div className="spacer" />
          {agent.plugin !== "external" && files.picker}
          {/* One button: stop while it works and nothing is typed, else send (which queues). */}
          {canInterrupt && !draft.trim() && !files.held.length ? (
            <button className="primary send" onClick={interrupt} title="中断本轮（Esc）" aria-label="中断本轮">
              <Icon name="stop" size={14} />
            </button>
          ) : (
            <button className="primary send" disabled={busy || (!draft.trim() && !files.held.length)} onClick={() => send()} aria-label="发送">
              <Icon name="send" size={16} />
            </button>
          )}
        </div>
      </div>
      <div className="composer-hint">
        {note && <span>{note} · </span>}
        {compact.busy ? "压缩中…" : composerHint({ running, steer, interrupt: canInterrupt })}
      </div>
    </div>
  );
}

/**
 * A plan waiting for approval, docked where the composer is (the draft is kept). Keys: 1
 * approve, 2 reject with a reason, 3 or Esc reply to the planner first.
 */
function PlanDock({ run, onDone, onReply }: { run: Run; onDone: () => void; onReply: () => void }) {
  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const ref = useRef<HTMLDivElement>(null);
  const choices = usePlanApproval(run, busy);

  useEffect(() => ref.current?.focus(), []);

  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      onDone();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }
  const approve = () => choices.ready && act(choices.approve);

  function onKey(e: KeyboardEvent<HTMLDivElement>) {
    if (rejecting || busy || e.target !== e.currentTarget) return;
    if (e.key === "1") approve();
    else if (e.key === "2") setRejecting(true);
    else if (e.key === "3" || e.key === "Escape") onReply();
    else return;
    e.preventDefault();
  }

  return (
    <div className="composer-wrap">
      <div className="plan-dock" tabIndex={-1} ref={ref} onKeyDown={onKey} role="dialog" aria-label="Plan 待审批">
        <header>
          <StateWord state={{ glyph: "○", word: "Plan 待审批", tone: "warn", bucket: "needs_you", live: false }} />
          <span className="muted small">{run.plan.length} 个任务</span>
          <span className="spacer" />
          <a className="small" href={`#/runs/${run.id}`}>
            {run.title}
          </a>
        </header>
        <div className="plan-dock-body">
          <PlanTasks run={run} pick={choices.pick} />
          {run.note && <div className="small muted plan-dock-note">上次驳回：{run.note}</div>}
          {choices.settings}
        </div>
        <footer>
          {rejecting ? (
            <>
              <textarea rows={2} autoFocus placeholder="驳回理由（必填），会转给 Planner" value={reason} onChange={(e) => setReason(e.target.value)} />
              <button className="danger" disabled={busy || !reason.trim()} onClick={() => act(() => api.rejectRun(run.id, { reason: reason.trim() }))}>
                驳回
              </button>
              <button className="ghost" onClick={() => setRejecting(false)}>
                取消
              </button>
            </>
          ) : (
            <>
              <button className="primary" disabled={busy || !choices.ready} onClick={approve}>
                <kbd>1</kbd> Approve · 开工
              </button>
              <button disabled={busy} onClick={() => setRejecting(true)}>
                <kbd>2</kbd> 驳回并说明
              </button>
              <button className="ghost" onClick={onReply}>
                <kbd>3</kbd> 先回复 Planner
              </button>
            </>
          )}
        </footer>
        {error && <ErrorBox error={error} />}
      </div>
    </div>
  );
}
