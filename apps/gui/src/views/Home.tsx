import { useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import type { Agent } from "@bindings/Agent";
import { api, errorText } from "../api";
import { navigate, useRunTree } from "../App";
import { Avatar, ErrorBox, Field, Icon, Vendor, agentTitle, displayName, useAgents } from "../components";
import { t } from "../lib/i18n";
import { PRESETS, presetPolicy, type Preset } from "../lib/policy";
import { QuestionCards } from "../questions";
import { useSelectedWorkspace, useWorkspaces, wsHref, wsKey } from "../workspace";

const PRESET_KEY = "crossagents.preset";

function savedPreset(): Preset {
  try {
    const v = localStorage.getItem(PRESET_KEY);
    if (v === "manual" || v === "semi" || v === "full") return v;
  } catch {
    // Storage may be unavailable; the default is fine.
  }
  return "manual";
}

/** Start page: say what you want, pick who plans, and who is on the team. */
export function HomeView() {
  const agents = useAgents();
  const tree = useRunTree();
  const scope = useSelectedWorkspace();
  const { list } = useWorkspaces();
  const current = list.find((w) => w.key === scope);
  // In a workspace, offer only its agents (a run works in its planner's directory).
  const all = (agents.data ?? []).filter((a) => !current || wsKey(a.cwd) === current.key);
  const [request, setRequest] = useState("");
  const [title, setTitle] = useState("");
  const [planner, setPlanner] = useState("");
  const [worker, setWorker] = useState("");
  const [escalate, setEscalate] = useState("");
  const [reviewer, setReviewer] = useState("");
  const [benched, setBenched] = useState<string[]>([]);
  const [maxParallel, setMaxParallel] = useState("");
  const [preset, setPresetState] = useState<Preset>(savedPreset);
  const [maxUsd, setMaxUsd] = useState("");
  const [maxMinutes, setMaxMinutes] = useState("");
  const [maxTokens, setMaxTokens] = useState("");
  const setPreset = (p: Preset) => {
    setPresetState(p);
    try {
      localStorage.setItem(PRESET_KEY, p);
    } catch {
      // Not remembered; fine.
    }
  };
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const ref = useRef<HTMLTextAreaElement>(null);

  const planners = all.filter((a) => a.role === "planner");
  const plannerAgent = planners.find((a) => a.name === planner) ?? planners[0];
  // The team works in the planner's repository; the hub picks the same people by default.
  const candidates = all.filter((a) => a.role !== "planner" && (!plannerAgent || wsKey(a.cwd) === wsKey(plannerAgent.cwd)));
  const team = candidates.filter((a) => !benched.includes(a.name));
  const workers = team.filter((a) => a.role === "worker");
  // Default to whoever was the default worker last time, not a specialist that sorts first.
  const lastDefault = tree.data?.find((d) => workers.some((w) => w.id === d.run.config.worker))?.run.config.worker;
  const workerAgent = workers.find((a) => a.name === worker) ?? workers.find((a) => a.id === lastDefault) ?? workers[0];
  const ready = !busy && !!request.trim() && !!plannerAgent && !!workerAgent;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 320)}px`;
  }, [request]);

  const toggle = (name: string) => setBenched((b) => (b.includes(name) ? b.filter((n) => n !== name) : [...b, name]));

  async function submit() {
    if (!ready) return;
    setBusy(true);
    setError(undefined);
    try {
      const run = await api.createRun({
        request: request.trim(),
        title: title.trim() || null,
        planner: plannerAgent.name,
        worker: workerAgent.name,
        escalate: escalate || null,
        reviewer: reviewer || null,
        auto_approve: false,
        max_parallel: maxParallel ? Number(maxParallel) : null,
        max_attempts: null,
        // Everyone in the repository is the hub's default; send a list only when trimmed.
        team: benched.length ? team.map((a) => a.name) : null,
        policy: {
          ...presetPolicy(preset),
          budget: {
            max_usd: maxUsd ? Number(maxUsd) : null,
            max_minutes: maxMinutes ? Number(maxMinutes) : null,
            max_tokens: maxTokens ? Math.round(Number(maxTokens) * 1000) : null,
          },
        },
      });
      navigate("runs", run.id);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  function onKey(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void submit();
    }
  }

  // Getting started: shown until there is a teammate, a planner and a first run.
  const steps = agents.data && tree.data && {
    mates: all.some((a) => a.role !== "planner"),
    planner: planners.length > 0,
    run: tree.data.length > 0,
  };

  return (
    <div className="page">
      <div className="page-inner home">
        <div className="empty" style={{ padding: "0 0 24px" }}>
          <h1>{t("home.title")}</h1>
          <p>
            {current ? (
              <>
                {t("home.in")} <a href={wsHref(current.key)}>{current.name}</a> ·{" "}
              </>
            ) : null}
            {t("home.subtitle", { hint: PRESETS.find((p) => p.key === preset)!.hint })}
          </p>
        </div>

        {steps && !(steps.mates && steps.planner && steps.run) && <GettingStarted steps={steps} onCompose={() => ref.current?.focus()} />}

        <QuestionCards agents={agents.data ?? []} />
        <div className="composer" style={{ maxWidth: "none" }}>
          <textarea
            ref={ref}
            rows={3}
            value={request}
            onChange={(e) => setRequest(e.target.value)}
            onKeyDown={onKey}
            placeholder={t("home.placeholder")}
            autoFocus
          />
          <div className="bar">
            <PersonPick label={t("home.planner")} agents={planners} value={plannerAgent} onChange={setPlanner} />
            <PersonPick label={t("home.worker")} agents={workers} value={workerAgent} onChange={setWorker} />
            <div className="spacer" />
            <span className="muted small hide-narrow">Ctrl+Enter</span>
            <button className="primary send" disabled={!ready} onClick={submit} aria-label={t("home.start")} title={t("home.startHint")}>
              <Icon name="send" size={16} />
            </button>
          </div>
        </div>

        <div className="preset-pick" role="radiogroup" aria-label={t("home.preset")}>
          {PRESETS.map((p) => (
            <button key={p.key} type="button" role="radio" aria-checked={preset === p.key} className={preset === p.key ? "on" : ""} onClick={() => setPreset(p.key)} title={p.hint}>
              {p.label}
            </button>
          ))}
        </div>

        {candidates.length > 0 && (
          <div className="team-pick">
            <div className="team-pick-label">
              {t("home.team")} <span className="muted">· {t("home.teamHint")}</span>
            </div>
            <div className="team-pick-list">
              {candidates.map((a) => {
                const on = !benched.includes(a.name);
                return (
                  <button
                    type="button"
                    key={a.id}
                    className={`mate ${on ? "on" : "off"}`}
                    onClick={() => toggle(a.name)}
                    aria-pressed={on}
                    title={a.profile.persona ?? undefined}
                  >
                    <Avatar agent={a} size={22} />
                    <span className="mate-who">
                      <span className="mate-name">{displayName(a)}</span>
                      <span className="mate-title">
                        {agentTitle(a)} · <Vendor plugin={a.plugin} model={a.model} />
                      </span>
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        <details className="more-options">
          <summary className="muted small">{t("home.more")}</summary>
          <div className="card form" style={{ marginTop: 10 }}>
            <Field label={t("home.titleField")}>
              <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t("home.titlePlaceholder")} />
            </Field>
            <div className="row" style={{ alignItems: "flex-start" }}>
              <Field label={t("home.escalate")}>
                <AgentSelect agents={workers} value={escalate} onChange={setEscalate} />
              </Field>
              <Field label={t("home.reviewer")}>
                <AgentSelect agents={team.filter((a) => a.role === "reviewer")} value={reviewer} onChange={setReviewer} />
              </Field>
              <Field label={t("home.parallel")}>
                <input type="number" min={1} value={maxParallel} onChange={(e) => setMaxParallel(e.target.value)} placeholder="3" />
              </Field>
            </div>
            <div className="row" style={{ alignItems: "flex-start" }}>
              <Field label={t("home.maxUsd")}>
                <input type="number" min={0} step="0.5" value={maxUsd} onChange={(e) => setMaxUsd(e.target.value)} placeholder={t("home.unlimited")} />
              </Field>
              <Field label={t("home.maxMinutes")}>
                <input type="number" min={1} value={maxMinutes} onChange={(e) => setMaxMinutes(e.target.value)} placeholder={t("home.unlimited")} />
              </Field>
              <Field label={t("home.maxTokens")}>
                <input type="number" min={1} value={maxTokens} onChange={(e) => setMaxTokens(e.target.value)} placeholder={t("home.unlimited")} />
              </Field>
            </div>
            <p className="muted small" style={{ margin: 0 }}>
              {t("home.budgetHint")}
            </p>
          </div>
        </details>
        {error && (
          <div style={{ marginTop: 12 }}>
            <ErrorBox error={error} />
          </div>
        )}
      </div>
    </div>
  );
}

/** Three steps to a first run; each one opens where it is done, done ones are ticked. */
function GettingStarted({ steps, onCompose }: { steps: { mates: boolean; planner: boolean; run: boolean }; onCompose: () => void }) {
  const items = [
    { done: steps.mates, icon: "addMate" as const, title: t("onboard.mates"), hint: t("onboard.matesHint"), href: "#/agents" },
    { done: steps.planner, icon: "planner" as const, title: t("onboard.planner"), hint: t("onboard.plannerHint"), href: "#/agents" },
    { done: steps.run, icon: "rocket" as const, title: t("onboard.run"), hint: t("onboard.runHint") },
  ];
  // The first step not done yet is the one to do now.
  const next = items.findIndex((s) => !s.done);
  return (
    <ol className="onboard" aria-label={t("onboard.label")}>
      {items.map((s, i) => {
        const body = (
          <>
            <span className={`onboard-mark ${s.done ? "done" : ""}`} aria-hidden="true">
              {s.done ? <Icon name="check" size={14} /> : i + 1}
            </span>
            <span className="onboard-text">
              <span className="onboard-title">
                <Icon name={s.icon} size={15} />
                {s.title}
              </span>
              <span className="onboard-hint">{s.hint}</span>
            </span>
          </>
        );
        const cls = `onboard-step ${s.done ? "done" : ""} ${i === next ? "next" : ""}`;
        return (
          <li key={i}>
            {s.href ? (
              <a className={cls} href={s.href} aria-current={i === next ? "step" : undefined}>
                {body}
              </a>
            ) : (
              <button type="button" className={cls} onClick={onCompose} aria-current={i === next ? "step" : undefined}>
                {body}
              </button>
            )}
          </li>
        );
      })}
    </ol>
  );
}

/** A person picker inside the composer: avatar, then a borderless select. */
function PersonPick({ label, agents, value, onChange }: { label: string; agents: Agent[]; value?: Agent; onChange: (v: string) => void }) {
  return (
    <label className="person-pick">
      <span className="pp-label">{label}</span>
      {value && <Avatar agent={value} size={18} />}
      <select value={value?.name ?? ""} onChange={(e) => onChange(e.target.value)}>
        {!value && <option value="">{t("common.none")}</option>}
        {agents.map((a) => (
          <option key={a.id} value={a.name}>
            {displayName(a)} · {agentTitle(a)}
          </option>
        ))}
      </select>
    </label>
  );
}

function AgentSelect({ agents, value, onChange }: { agents: Agent[]; value: string; onChange: (v: string) => void }) {
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      <option value="">{t("home.notUsed")}</option>
      {agents.map((a) => (
        <option key={a.id} value={a.name}>
          {displayName(a)} · {agentTitle(a)} · {a.model ?? a.plugin}
        </option>
      ))}
    </select>
  );
}
