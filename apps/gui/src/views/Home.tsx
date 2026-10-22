import { useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { PluginInfo } from "@bindings/PluginInfo";
import { api, errorText } from "../api";
import { navigate, useRunTree } from "../App";
import { Avatar, BrandMark, ErrorBox, Field, Icon, VendorMark, agentTitle, displayName, hueOf, useAgents } from "../components";
import { useHubData } from "../hub";
import { t } from "../lib/i18n";
import { agentNameFor, chatNameFor, chatTitle, prettyModel, vendorLabel, workerModelFor } from "../lib/models";
import { PRESETS, presetPolicy, type Preset } from "../lib/policy";
import { ChipMenu, type MenuItem } from "../menu";
import { QuestionCards } from "../questions";
import { CHATS, selectWorkspace, useSelectedWorkspace, useWorkspaces, wsKey, wsName } from "../workspace";

const PRESET_KEY = "crossagents.preset";
const LEAD_KEY = "crossagents.lead";
const MODE_KEY = "crossagents.mode";
const CHAT_MODEL_KEY = "crossagents.chatModel";

/** Who leads a new run: an agent that exists, or a model to make one for. */
type Lead = { kind: "agent"; name: string } | { kind: "model"; plugin: string; model: string };

/** A chat with one agent, or a run a lead plans and a team carries out. */
type Mode = "chat" | "run";

function load<T>(key: string, fallback: T, ok: (v: unknown) => boolean): T {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(key) ?? "null");
    return ok(v) ? (v as T) : fallback;
  } catch {
    return fallback;
  }
}

function save(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Not remembered; fine.
  }
}

/**
 * Start page, quiet like Claude Code's: a greeting and one composer. Where (a workspace) and
 * who leads (a model or an agent) are chips in the composer. Picking a model is enough: the
 * page reuses or creates the agents the run needs, and the lead recruits the rest itself.
 * Everything else waits behind the options button.
 */
export function HomeView() {
  const agents = useAgents();
  const tree = useRunTree();
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const scope = useSelectedWorkspace();
  const { list: all } = useWorkspaces();
  // Projects only: the chats group is not a directory to work in.
  const list = all.filter((w) => w.key !== CHATS);
  const current = list.find((w) => w.key === scope);
  const [folder, setFolder] = useState<string>();
  const cwd = folder ?? current?.path;
  const [mode, setModeState] = useState<Mode>(() => load<Mode>(MODE_KEY, "chat", (v) => v === "chat" || v === "run"));
  const setMode = (m: Mode) => {
    setModeState(m);
    save(MODE_KEY, m);
  };
  const [chatPick, setChatPick] = useState<{ plugin: string; model: string } | undefined>(() =>
    load(CHAT_MODEL_KEY, undefined, (v) => !!v && typeof v === "object"),
  );
  const everyone = agents.data ?? [];
  // In a workspace, offer only its agents (a run works in its planner's directory).
  const here = everyone.filter((a) => !cwd || wsKey(a.cwd) === wsKey(cwd));
  const [request, setRequest] = useState("");
  const [title, setTitle] = useState("");
  const [picked, setPicked] = useState<Lead | undefined>(() => load<Lead | undefined>(LEAD_KEY, undefined, (v) => !!v && typeof v === "object"));
  const [worker, setWorker] = useState("");
  const [escalate, setEscalate] = useState("");
  const [reviewer, setReviewer] = useState("");
  const [maxParallel, setMaxParallel] = useState("");
  const [preset, setPresetState] = useState<Preset>(() => load<Preset>(PRESET_KEY, "manual", (v) => v === "manual" || v === "semi" || v === "full"));
  const [maxUsd, setMaxUsd] = useState("");
  const [maxMinutes, setMaxMinutes] = useState("");
  const [maxTokens, setMaxTokens] = useState("");
  const [options, setOptions] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const ref = useRef<HTMLTextAreaElement>(null);

  const setPreset = (p: Preset) => {
    setPresetState(p);
    save(PRESET_KEY, p);
  };
  const choose = (lead: Lead) => {
    setPicked(lead);
    save(LEAD_KEY, lead);
  };

  const planners = here.filter((a) => a.role === "planner");
  const offered = (plugins.data ?? []).filter((p) => p.models.length > 0);
  // The remembered pick when it still exists here, else the first planner, else the first model.
  const lead: Lead | undefined =
    picked?.kind === "agent" && planners.some((a) => a.name === picked.name)
      ? picked
      : picked?.kind === "model" && offered.some((p) => p.name === picked.plugin && p.models.includes(picked.model))
        ? picked
        : planners[0]
          ? { kind: "agent", name: planners[0].name }
          : offered[0]
            ? { kind: "model", plugin: offered[0].name, model: offered[0].models[0] }
            : undefined;
  const leadAgent = lead?.kind === "agent" ? planners.find((a) => a.name === lead.name) : undefined;
  const leadPlugin = leadAgent?.plugin ?? (lead?.kind === "model" ? lead.plugin : undefined);
  const leadModel = leadAgent?.model ?? (lead?.kind === "model" ? lead.model : undefined);
  const where = leadAgent?.cwd ?? cwd;

  const team = here.filter((a) => a.role !== "planner" && (!where || wsKey(a.cwd) === wsKey(where)));
  const workers = team.filter((a) => a.role === "worker");
  const lastDefault = tree.data?.find((d) => workers.some((w) => w.id === d.run.config.worker))?.run.config.worker;
  const chosenWorker = workers.find((a) => a.name === worker) ?? workers.find((a) => a.id === lastDefault) ?? workers[0];
  // A chat needs only a model: the remembered one if the hub still offers it, else the first.
  const chatModel =
    chatPick && offered.some((p) => p.name === chatPick.plugin && p.models.includes(chatPick.model))
      ? chatPick
      : offered[0]
        ? { plugin: offered[0].name, model: offered[0].models[0] }
        : undefined;
  const ready = !busy && !!request.trim() && (mode === "chat" ? !!chatModel : !!lead && !!where);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 320)}px`;
  }, [request]);

  /** An agent with this plugin, model and role in `dir`, or a new one named after the model. */
  async function agentFor(role: "planner" | "worker", plugin: string, model: string, dir: string): Promise<Agent> {
    const found = everyone.find((a) => a.role === role && a.plugin === plugin && a.model === model && wsKey(a.cwd) === wsKey(dir));
    if (found) return found;
    return api.createAgent({
      name: agentNameFor(role, model, new Set(everyone.map((a) => a.name))),
      plugin,
      driver: null,
      model,
      role,
      cwd: dir,
      config: {},
      profile: { display_name: prettyModel(model), title: role === "planner" ? t("home.leadTitle") : "Worker", avatar: null, color: null, persona: null },
    });
  }

  /** A chat is a new agent (named after the model, titled by the message) and its first message. */
  async function startChat() {
    if (!ready || !chatModel) return;
    setBusy(true);
    setError(undefined);
    try {
      const text = request.trim();
      const agent = await api.createAgent({
        name: chatNameFor(chatModel.model, new Set(everyone.map((a) => a.name))),
        plugin: chatModel.plugin,
        driver: null,
        model: chatModel.model,
        role: "worker",
        // No project: the hub gives the chat its own directory.
        cwd: cwd ?? "",
        config: {},
        profile: { display_name: chatTitle(text), title: prettyModel(chatModel.model), avatar: null, color: null, persona: null },
      });
      await api.sendMessage(agent.name, { text });
      navigate("s", agent.name);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  async function submit() {
    if (mode === "chat") return startChat();
    if (!ready || !lead || !where) return;
    setBusy(true);
    setError(undefined);
    try {
      const planner = leadAgent ?? (await agentFor("planner", leadPlugin!, leadModel!, where));
      const models = plugins.data?.find((p) => p.name === planner.plugin)?.models ?? [];
      const workerAgent = chosenWorker ?? (await agentFor("worker", planner.plugin, workerModelFor(models, planner.model ?? leadModel ?? models[0] ?? ""), where));
      const run = await api.createRun({
        request: request.trim(),
        title: title.trim() || null,
        planner: planner.name,
        worker: workerAgent.name,
        escalate: escalate || null,
        reviewer: reviewer || null,
        auto_approve: false,
        max_parallel: maxParallel ? Number(maxParallel) : null,
        max_attempts: null,
        team: null,
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

  const pickFolder = window.crossagents?.pickFolder;
  const wsItems: MenuItem[] = [
    // A chat may live outside any project; a run always works in one.
    ...(mode === "chat"
      ? [
          {
            kind: "item",
            key: "none",
            label: t("home.noProject"),
            sub: t("home.noProjectHint"),
            icon: <Icon name="chat" size={14} />,
            on: !folder && !current,
            onPick: () => {
              setFolder(undefined);
              selectWorkspace("");
            },
          } satisfies MenuItem,
        ]
      : []),
    ...list.map(
      (w): MenuItem => ({
        kind: "item",
        key: w.key,
        label: w.name,
        sub: w.path,
        icon: <span className={`ws-dot av-${hueOf(w.key)}`} />,
        on: !folder && current?.key === w.key,
        onPick: () => {
          setFolder(undefined);
          selectWorkspace(w.key);
        },
      }),
    ),
    ...(pickFolder
      ? [
          {
            kind: "item",
            key: "pick",
            label: t("home.pickFolder"),
            icon: <Icon name="folder" size={14} />,
            onPick: async () => {
              const dir = await pickFolder();
              if (!dir) return;
              const known = list.find((w) => w.key === wsKey(dir));
              if (known) {
                setFolder(undefined);
                selectWorkspace(known.key);
              } else setFolder(dir);
            },
          } satisfies MenuItem,
        ]
      : []),
  ];

  const leadItems: MenuItem[] = [
    ...offered.flatMap((p: PluginInfo): MenuItem[] => [
      { kind: "header", key: `h-${p.name}`, label: vendorLabel(p.name) },
      ...p.models.map(
        (m): MenuItem => ({
          kind: "item",
          key: `${p.name}/${m}`,
          label: prettyModel(m),
          sub: m,
          icon: <VendorMark plugin={p.name} size={14} />,
          on: lead?.kind === "model" && lead.plugin === p.name && lead.model === m,
          onPick: () => choose({ kind: "model", plugin: p.name, model: m }),
        }),
      ),
    ]),
    ...(planners.length ? [{ kind: "header", key: "h-agents", label: t("home.existing") } satisfies MenuItem] : []),
    ...planners.map(
      (a): MenuItem => ({
        kind: "item",
        key: `a/${a.name}`,
        label: displayName(a),
        sub: `${agentTitle(a)} · ${a.model ?? a.plugin}`,
        icon: <Avatar agent={a} size={18} />,
        on: lead?.kind === "agent" && lead.name === a.name,
        onPick: () => choose({ kind: "agent", name: a.name }),
      }),
    ),
  ];

  const chatItems: MenuItem[] = offered.flatMap((p: PluginInfo): MenuItem[] => [
    { kind: "header", key: `h-${p.name}`, label: vendorLabel(p.name) },
    ...p.models.map(
      (m): MenuItem => ({
        kind: "item",
        key: `${p.name}/${m}`,
        label: prettyModel(m),
        sub: m,
        icon: <VendorMark plugin={p.name} size={14} />,
        on: chatModel?.plugin === p.name && chatModel.model === m,
        onPick: () => {
          setChatPick({ plugin: p.name, model: m });
          save(CHAT_MODEL_KEY, { plugin: p.name, model: m });
        },
      }),
    ),
  ]);

  const wsLabel = folder ? wsName(folder) : current?.name;
  const missing =
    !agents.data || !plugins.data
      ? undefined
      : !offered.length
        ? t("home.noModels")
        : mode === "run" && !where
          ? t("home.needWorkspace")
          : undefined;

  return (
    <div className="page">
      <div className="page-inner home">
        <div className="hero">
          <BrandMark size={30} />
          <h1>{t("home.title")}</h1>
          <div className="segmented mode-pick" role="radiogroup" aria-label={t("home.mode")}>
            <button className={mode === "chat" ? "active" : ""} role="radio" aria-checked={mode === "chat"} onClick={() => setMode("chat")} title={t("home.chatHint")}>
              <Icon name="chat" size={14} />
              {t("home.chat")}
            </button>
            <button className={mode === "run" ? "active" : ""} role="radio" aria-checked={mode === "run"} onClick={() => setMode("run")} title={t("home.runHint")}>
              <Icon name="team" size={14} />
              {t("home.run")}
            </button>
          </div>
        </div>

        <QuestionCards agents={everyone} />
        <div className="composer home-composer">
          <textarea
            ref={ref}
            rows={3}
            value={request}
            onChange={(e) => setRequest(e.target.value)}
            onKeyDown={onKey}
            placeholder={t(mode === "chat" ? "home.chatPlaceholder" : "home.placeholder")}
            autoFocus
          />
          <div className="bar">
            <ChipMenu
              icon={wsLabel ? <span className={`ws-dot av-${hueOf(wsKey(folder ?? current!.path))}`} /> : <Icon name={mode === "chat" ? "chat" : "folder"} size={14} />}
              label={wsLabel ?? t(mode === "chat" ? "home.noProject" : "home.chooseWorkspace")}
              title={folder ?? current?.path}
              items={wsItems}
              up
            />
            {mode === "chat" && chatModel && (
              <ChipMenu
                icon={<VendorMark plugin={chatModel.plugin} size={14} />}
                label={`${vendorLabel(chatModel.plugin)} ${prettyModel(chatModel.model)}`}
                title={t("home.model")}
                items={chatItems}
                up
              />
            )}
            {mode === "run" && lead && (
              <ChipMenu
                icon={leadAgent ? <Avatar agent={leadAgent} size={16} /> : <VendorMark plugin={leadPlugin!} size={14} />}
                label={leadAgent ? displayName(leadAgent) : `${vendorLabel(leadPlugin!)} ${prettyModel(leadModel!)}`}
                title={t("home.lead")}
                items={leadItems}
                up
              />
            )}
            <div className="spacer" />
            {mode === "run" && (
              <button
                type="button"
                className={`icon-btn options-btn ${options ? "on" : ""}`}
                onClick={() => setOptions(!options)}
                aria-expanded={options}
                title={t("home.options")}
                aria-label={t("home.options")}
              >
                <Icon name="sliders" size={15} />
              </button>
            )}
            <button
              className="primary send"
              disabled={!ready}
              onClick={submit}
              aria-label={t(mode === "chat" ? "home.send" : "home.start")}
              title={t(mode === "chat" ? "home.sendHint" : "home.startHint")}
            >
              <Icon name="send" size={16} />
            </button>
          </div>
        </div>

        {missing && <p className="home-note">{missing}</p>}

        {mode === "run" && options && (
          <div className="card form home-options">
            <div className="row" style={{ alignItems: "flex-start" }}>
              <Field label={t("home.preset")}>
                <select value={preset} onChange={(e) => setPreset(e.target.value as Preset)} title={PRESETS.find((p) => p.key === preset)?.hint}>
                  {PRESETS.map((p) => (
                    <option key={p.key} value={p.key} title={p.hint}>
                      {p.label}
                    </option>
                  ))}
                </select>
              </Field>
              <Field label={t("home.titleField")}>
                <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t("home.titlePlaceholder")} />
              </Field>
            </div>
            <div className="row" style={{ alignItems: "flex-start" }}>
              <Field label={t("home.worker")}>
                <AgentSelect agents={workers} value={chosenWorker?.name ?? ""} onChange={setWorker} empty={t("home.auto")} />
              </Field>
              <Field label={t("home.escalate")}>
                <AgentSelect agents={workers} value={escalate} onChange={setEscalate} empty={t("home.notUsed")} />
              </Field>
              <Field label={t("home.reviewer")}>
                <AgentSelect agents={team.filter((a) => a.role === "reviewer")} value={reviewer} onChange={setReviewer} empty={t("home.notUsed")} />
              </Field>
            </div>
            <div className="row" style={{ alignItems: "flex-start" }}>
              <Field label={t("home.parallel")}>
                <input type="number" min={1} value={maxParallel} onChange={(e) => setMaxParallel(e.target.value)} placeholder="3" />
              </Field>
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
          </div>
        )}
        {error && (
          <div style={{ marginTop: 12 }}>
            <ErrorBox error={error} />
          </div>
        )}
      </div>
    </div>
  );
}

function AgentSelect({ agents, value, onChange, empty }: { agents: Agent[]; value: string; onChange: (v: string) => void; empty: string }) {
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      <option value="">{empty}</option>
      {agents.map((a) => (
        <option key={a.id} value={a.name}>
          {displayName(a)} · {agentTitle(a)} · {a.model ?? a.plugin}
        </option>
      ))}
    </select>
  );
}
