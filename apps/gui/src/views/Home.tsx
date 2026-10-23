import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { PluginInfo } from "@bindings/PluginInfo";
import { api, errorText } from "../api";
import { navigate, useRunTree } from "../App";
import { Avatar, BrandMark, ErrorBox, Field, Icon, VendorMark, agentTitle, displayName, hueOf, useAgents } from "../components";
import { useHubData } from "../hub";
import { t } from "../lib/i18n";
import { chatTitle, effortLabel, effortLevels, effortOf, prettyModel, vendorLabel, workerModelFor } from "../lib/models";
import { useQuota } from "./Quota";
import { PRESETS, presetPolicy, type Preset } from "../lib/policy";
import { ChipMenu, type MenuItem } from "../menu";
import { QuestionCards } from "../questions";
import { CHATS, selectWorkspace, useSelectedWorkspace, useWorkspaces, wsKey, wsName } from "../workspace";

const PRESET_KEY = "crossagents.preset";
const LEAD_KEY = "crossagents.lead";
const MODE_KEY = "crossagents.mode";
const CHAT_MODEL_KEY = "crossagents.chatModel";
const EFFORT_KEY = "crossagents.chatEffort";

/** Who leads a new run: an agent that exists, or a model to make one for. */
type Lead = { kind: "agent"; name: string } | ({ kind: "model" } & Choice);

/** A model to run on, and which login of its vendor pays for it (absent: the default one). */
type Choice = { plugin: string; model: string; account?: string };

/** The config a new agent on `choice` starts with: its account and reasoning effort. */
const configFor = (choice: Choice, effort?: string) => ({ ...(choice.account ? { account: choice.account } : {}), ...(effort ? { effort } : {}) });

const accountOf = (a: Agent) => {
  const v = a.config && typeof a.config === "object" ? (a.config as Record<string, unknown>).account : undefined;
  return typeof v === "string" ? v : undefined;
};

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
  // 文件 → 新建对话 / 新建团队 run while this page is open (menubar.tsx `startNew`).
  useEffect(() => {
    const onMode = (e: Event) => {
      const m = (e as CustomEvent<unknown>).detail;
      if (m === "chat" || m === "run") setModeState(m);
    };
    window.addEventListener("crossagents:mode", onMode);
    return () => window.removeEventListener("crossagents:mode", onMode);
  }, []);
  const [chatPick, setChatPick] = useState<Choice | undefined>(() => load(CHAT_MODEL_KEY, undefined, (v) => !!v && typeof v === "object"));
  // The reasoning effort picked in the composer, for a chat's agent or a run's lead.
  const [effortPick, setEffortState] = useState<string>(() => load(EFFORT_KEY, "", (v) => typeof v === "string"));
  const setEffort = (e: string) => {
    setEffortState(e);
    save(EFFORT_KEY, e);
  };
  // Logins per vendor: with more than one, the model menus also say which login pays.
  const quota = useQuota();
  const accountsOf = (plugin: string) => (quota.data?.accounts ?? []).filter((a) => a.account.provider === plugin);
  const accountLabel = (id?: string) => {
    const a = quota.data?.accounts.find((x) => x.account.id === id);
    return a ? a.account.label || a.account.id : undefined;
  };
  // Archived conversations are not picked again.
  const everyone = (agents.data ?? []).filter((a) => !a.archived_at);
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
  // Effort levels of the vendor picked: the chat's model, or the run's lead. A pick the
  // vendor does not offer falls back to its default; an existing lead shows its own.
  const levels = effortLevels(plugins.data?.find((p) => p.name === (mode === "chat" ? chatModel?.plugin : leadPlugin))?.efforts);
  const effortWanted = effortPick || (mode === "run" && leadAgent ? effortOf(leadAgent.config) ?? "" : "");
  const effortOn = levels.some((l) => l.value === effortWanted) ? effortWanted : "";
  const effort = effortOn || undefined;

  /** `agent` on `effort`: switched to it first when it asks for another (none means as it is). */
  async function withEffort(agent: Agent, effort: string | undefined): Promise<Agent> {
    if (!effort || effortOf(agent.config) === effort) return agent;
    return api.setModel(agent.id, agent.model, effort);
  }

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 320)}px`;
  }, [request]);

  /** An agent with this role, model and login in `dir`, or a new one (the hub names it: a person's name, never the model's). */
  async function agentFor(role: "planner" | "worker", choice: Choice, dir: string, effort?: string): Promise<Agent> {
    const { plugin, model, account } = choice;
    const found = everyone.find((a) => a.role === role && a.plugin === plugin && a.model === model && accountOf(a) === account && wsKey(a.cwd) === wsKey(dir));
    if (found) return withEffort(found, effort);
    return api.createAgent({
      plugin,
      driver: null,
      model,
      role,
      cwd: dir,
      config: configFor(choice, effort),
      profile: { display_name: null, title: role === "planner" ? t("home.leadTitle") : "Worker", avatar: null, color: null, persona: null },
    });
  }

  /** A chat is a new agent (named by the hub, the conversation titled by the message) and its first message. */
  async function startChat() {
    if (!ready || !chatModel) return;
    setBusy(true);
    setError(undefined);
    try {
      const text = request.trim();
      const agent = await api.createAgent({
        topic: chatTitle(text, 60),
        plugin: chatModel.plugin,
        driver: null,
        model: chatModel.model,
        role: "worker",
        // No project: the hub gives the chat its own directory.
        cwd: cwd ?? "",
        config: configFor(chatModel, effort),
        profile: { display_name: null, title: null, avatar: null, color: null, persona: null },
      });
      await api.sendMessage(agent.id, { text });
      navigate("s", agent.id);
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
      const leadChoice: Choice = lead.kind === "model" ? lead : { plugin: leadPlugin!, model: leadModel!, account: leadAgent && accountOf(leadAgent) };
      const planner = leadAgent ? await withEffort(leadAgent, effort) : await agentFor("planner", leadChoice, where, effort);
      const models = plugins.data?.find((p) => p.name === planner.plugin)?.models ?? [];
      // Workers run on the lead's login, on the cheaper sibling of its model.
      const workerChoice: Choice = { plugin: planner.plugin, model: workerModelFor(models, planner.model ?? leadModel ?? models[0] ?? ""), account: accountOf(planner) };
      const workerAgent = chosenWorker ?? (await agentFor("worker", workerChoice, where));
      const run = await api.createRun({
        request: request.trim(),
        title: title.trim() || null,
        planner: planner.id,
        worker: workerAgent.id,
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

  /**
   * Models by vendor; a vendor with several logins gets a group per login, so picking a model
   * also picks who pays for it.
   */
  const modelMenu = (isOn: (c: Choice) => boolean, onPick: (c: Choice) => void): MenuItem[] =>
    offered.flatMap((p: PluginInfo): MenuItem[] => {
      const logins = accountsOf(p.name);
      const groups = logins.length > 1 ? logins.map((a) => ({ account: a.account.id, title: `${vendorLabel(p.name)} · ${a.account.label || a.account.id}` })) : [{ account: undefined, title: vendorLabel(p.name) }];
      return groups.flatMap(({ account, title }): MenuItem[] => [
        { kind: "header", key: `h-${p.name}-${account ?? ""}`, label: title },
        ...p.models.map((m): MenuItem => {
          const choice: Choice = { plugin: p.name, model: m, ...(account ? { account } : {}) };
          return { kind: "item", key: `${p.name}/${account ?? ""}/${m}`, label: prettyModel(m), sub: m, icon: <VendorMark plugin={p.name} size={14} />, on: isOn(choice), onPick: () => onPick(choice) };
        }),
      ]);
    });
  const same = (a: Choice | undefined, b: Choice) => !!a && a.plugin === b.plugin && a.model === b.model && (a.account ?? "") === (b.account ?? "");

  const leadItems: MenuItem[] = [
    ...modelMenu(
      (c) => lead?.kind === "model" && same(lead, c),
      (c) => choose({ kind: "model", ...c }),
    ),
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

  const chatItems = modelMenu(
    (c) => same(chatModel, c),
    (c) => {
      setChatPick(c);
      save(CHAT_MODEL_KEY, c);
    },
  );
  const effortItems: MenuItem[] = [
    { kind: "header", key: "h", label: t("home.effort") },
    { kind: "item", key: "", label: t("home.effortDefault"), on: !effortOn, onPick: () => setEffort("") },
    ...levels.map((l): MenuItem => ({ kind: "item", key: l.value, label: l.label, sub: l.value, on: effortOn === l.value, onPick: () => setEffort(l.value) })),
  ];
  /** "Claude Opus 5.5", plus the login when the vendor has more than one. */
  const choiceLabel = (c: Choice) => {
    const login = accountsOf(c.plugin).length > 1 ? accountLabel(c.account) ?? t("home.defaultLogin") : undefined;
    return `${vendorLabel(c.plugin)} ${prettyModel(c.model)}${login ? ` · ${login}` : ""}`;
  };

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
                label={choiceLabel(chatModel)}
                title={t("home.model")}
                items={chatItems}
                up
              />
            )}
            {mode === "run" && lead && (
              <ChipMenu
                icon={leadAgent ? <Avatar agent={leadAgent} size={16} /> : <VendorMark plugin={leadPlugin!} size={14} />}
                label={leadAgent ? displayName(leadAgent) : lead.kind === "model" ? choiceLabel(lead) : ""}
                title={t("home.lead")}
                items={leadItems}
                up
              />
            )}
            {/* After the model it applies to: the chat's, or the run lead's. */}
            {levels.length > 0 && (
              <ChipMenu
                icon={<Icon name="brain" size={13} />}
                label={`${t("home.effortShort")} ${effortOn ? effortLabel(effortOn) : t("home.effortDefault")}`}
                title={t("home.effort")}
                items={effortItems}
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
