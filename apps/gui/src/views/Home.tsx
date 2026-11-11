import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { PluginInfo } from "@bindings/PluginInfo";
import type { SessionCandidate } from "@bindings/SessionCandidate";
import { api, ApiError, errorText } from "../api";
import { HeldFiles, useHeld } from "../attach";
import { sendMessage } from "../outbox";
import { navigate } from "../App";
import { Avatar, BrandMark, ErrorBox, Field, Icon, VendorMark, agentLine, agentTitle, displayName, hueOf, relationText, sessionLabel, useAgents } from "../components";
import { useHubData } from "../hub";
import { useConnectionState } from "../lib/useConnection";
import { t } from "../lib/i18n";
import { modelDisplay } from "../lib/modelDisplay";
import { cursorMode, reasoningModels } from "../lib/reasoning";
import { ReasoningPicks } from "../reasoningPicks";
import { reasoningWorkerRequest, type ReasoningChoice } from "../lib/workerModels";
import { effortLabel, effortLevels, effortOf, prettyModel, vendorLabel, workerModelFor, accountOf } from "../lib/models";
import { chatBody, newChatRequest, runSessionFields, sessionStateKey, usabilityOf } from "../lib/sessionChoice";
import { configWithPermission, permissionOf, type PermissionChoice } from "../lib/permission";
import { PermissionPicks, applyPermission } from "../permissionPicks";
import * as draft from "../lib/draft";
import { useQuota } from "./Quota";
import { PRESETS, presetPolicy, type Preset } from "../lib/policy";
import { ChipMenu, type MenuItem } from "../menu";
import { mentionsFor, useMention } from "../mention";
import { QuestionCards } from "../questions";
import { CHATS, selectWorkspace, useSelectedWorkspace, useWorkspaces, wsKey, wsName } from "../workspace";

const DRAFT: draft.Where = { kind: "home" };
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
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const scope = useSelectedWorkspace();
  const { list: all } = useWorkspaces();
  // Projects only: the chats group is not a directory to work in.
  const list = all.filter((w) => w.key !== CHATS);
  const current = list.find((w) => w.key === scope);
  const [folder, setFolder] = useState<string>();
  // A remote hub's folders are on the centre's machine: there is no system dialog for them,
  // so the person types the path and the hub checks it when the agent is created.
  const remote = useConnectionState()?.mode === "remote";
  const [pathEntry, setPathEntry] = useState(false);
  const [manualPath, setManualPath] = useState("");
  const cwd = folder ?? current?.path;
  const [mode, setModeState] = useState<Mode>(() => load<Mode>(MODE_KEY, "chat", (v) => v === "chat" || v === "run"));
  const setMode = (m: Mode) => {
    setModeState(m);
    save(MODE_KEY, m);
  };
  // 文件 → 新建 Chat / 新建 Work while this page is open (menubar.tsx `startNew`).
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
  /**
   * One login's models (what it reported), else the vendor's; a list set in config.toml
   * wins. Narrowed to the models picked in settings, unless that leaves this login none.
   */
  const modelsFor = (p: PluginInfo, account?: string): string[] => {
    if (p.models_pinned || !account) return p.models;
    const own = quota.data?.accounts.find((a) => a.account.id === account)?.models.map((m) => m.id) ?? [];
    const picked = p.shown ? own.filter((m) => p.shown!.includes(m)) : own;
    return picked.length ? picked : p.models;
  };
  const accountLabel = (id?: string) => {
    const a = quota.data?.accounts.find((x) => x.account.id === id);
    return a ? a.account.label || a.account.id : undefined;
  };
  // Archived conversations are not picked again.
  const everyone = (agents.data ?? []).filter((a) => !a.archived_at);
  // In a workspace, offer only its agents (a run works in its planner's directory).
  const here = everyone.filter((a) => !cwd || wsKey(a.cwd) === wsKey(cwd));
  // What was typed last time this page was open: leaving it unmounts the composer, and a
  // half-written request with its screenshots should not go with it (lib/draft.ts).
  const kept = useRef(draft.read(DRAFT));
  const [request, setRequest] = useState(kept.current?.text ?? "");
  const [title, setTitle] = useState(kept.current?.title ?? "");
  const [draftNote, setDraftNote] = useState<string>();
  const [picked, setPicked] = useState<Lead | undefined>(() => load<Lead | undefined>(LEAD_KEY, undefined, (v) => !!v && typeof v === "object"));
  const [worker, setWorker] = useState("");
  // 危险操作怎么处理, for the chat's agent or the run's default worker. Left alone it follows
  // whoever is chosen (a new chat or an auto-picked worker follows the run / default).
  const [chatPerm, setChatPerm] = useState<PermissionChoice>();
  const [workPerm, setWorkPerm] = useState<PermissionChoice>();
  const [workerReasoning, setWorkerReasoning] = useState<{ key: string; choice: ReasoningChoice }>();
  const [escalate, setEscalate] = useState("");
  const [reviewer, setReviewer] = useState("");
  const [continuation, setContinuation] = useState(false);
  const [continuationAttempts, setContinuationAttempts] = useState(3);
  const [continuationInterval, setContinuationInterval] = useState(60);
  const [maxParallel, setMaxParallel] = useState("");
  const [preset, setPresetState] = useState<Preset>(() => load<Preset>(PRESET_KEY, "manual", (v) => v === "manual" || v === "semi" || v === "full"));
  const [maxUsd, setMaxUsd] = useState("");
  const [maxMinutes, setMaxMinutes] = useState("");
  const [maxTokens, setMaxTokens] = useState("");
  const [options, setOptions] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  // Screenshots and files for the first message: the chat's, or the run lead's kickoff. What
  // the agent takes is only known once it exists; the hub checks then and says why.
  const files = useHeld(setError);
  const ref = useRef<HTMLTextAreaElement>(null);

  // The files come back once, after `useHeld` exists; the text was restored above.
  useEffect(() => {
    if (kept.current?.held.length) files.restore(kept.current.held);
    kept.current = undefined;
  }, []);

  // Kept as you type, a moment after you stop. Images can be too big for the browser's
  // storage: then the text is kept alone and the composer says so rather than losing it
  // silently.
  useEffect(() => {
    const timer = setTimeout(() => {
      const out = draft.write(DRAFT, { text: request, title, held: files.held });
      setDraftNote(!out.kept && files.held.length ? t("draft.notKept") : out.dropped ? t("draft.filesTooBig", { n: out.dropped }) : undefined);
    }, 400);
    return () => clearTimeout(timer);
  }, [request, title, files.held]);

  /** Sent: the draft is done with. */
  const sent = () => {
    draft.forget(DRAFT);
    setDraftNote(undefined);
    files.clear();
  };

  // The delegation level carries over, 全托管 included (docs/tasks/W1.md §7).
  const setPreset = (p: Preset) => {
    setPresetState(p);
    save(PRESET_KEY, p);
  };
  const choose = (lead: Lead) => {
    setSessionId(undefined);
    setPicked(lead);
    save(LEAD_KEY, lead);
  };

  const planners = here.filter((a) => a.role === "planner");
  const offered = (plugins.data ?? []).filter((p) => p.models.length > 0);
  // The workspace whose sessions are offered: the picked one, or — for Work — the directory
  // the first planner works in, since a run starts there without picking a workspace chip.
  const baseLeadAgent = picked?.kind === "agent" ? planners.find((a) => a.name === picked.name) : planners[0];
  const sessionRepo = cwd ?? (mode === "run" && picked?.kind !== "model" ? baseLeadAgent?.cwd : undefined);
  // A chat with no workspace carries on one of the standalone chats, each in its own directory.
  const chatsOnly = !sessionRepo && mode === "chat";
  const sessions = useHubData(
    () =>
      sessionRepo || chatsOnly
        ? api.sessionCandidates(sessionRepo).then((r) => r.sessions)
        : Promise.resolve([] as SessionCandidate[]),
    [sessionRepo, chatsOnly],
    (e) => e.type.startsWith("agent.") || e.type === "run.status_changed" || e.type === "task.handed_off",
  );
  // The session picked to carry this start; absent: a new one, as before.
  const [sessionId, setSessionId] = useState<string>();
  useEffect(() => setSessionId(undefined), [sessionRepo, chatsOnly]);
  useEffect(() => {
    if (sessionId && sessions.data && !sessions.data.some((s) => s.agent.id === sessionId)) setSessionId(undefined);
  }, [sessions.data, sessionId]);
  const chosenSession = sessionId ? sessions.data?.find((s) => s.agent.id === sessionId) : undefined;
  // The permission pick follows whoever carries this start: another session or worker means
  // its own setting, until the person picks one here.
  useEffect(() => setChatPerm(undefined), [chosenSession?.agent.id]);
  const sessionUse = chosenSession ? usabilityOf(chosenSession) : undefined;
  // A picked session decides who leads; otherwise the remembered pick when it still exists
  // here, else the first planner, else the first model.
  const lead: Lead | undefined = chosenSession
    ? { kind: "agent", name: chosenSession.agent.name }
    : picked?.kind === "agent" && planners.some((a) => a.name === picked.name)
      ? picked
      : picked?.kind === "model" && offered.some((p) => p.name === picked.plugin && p.models.includes(picked.model))
        ? picked
        : planners[0]
          ? { kind: "agent", name: planners[0].name }
          : offered[0]
            ? { kind: "model", plugin: offered[0].name, model: offered[0].models[0] }
            : undefined;
  const leadAgent = chosenSession?.agent ?? (lead?.kind === "agent" ? planners.find((a) => a.name === lead.name) : undefined);
  const leadPlugin = leadAgent?.plugin ?? (lead?.kind === "model" ? lead.plugin : undefined);
  const leadModel = leadAgent?.model ?? (lead?.kind === "model" ? lead.model : undefined);
  const where = leadAgent?.cwd ?? cwd;

  // A session that leads must not also be the default worker. Any other may work or review:
  // a role says how a session was made, not what it may do (docs/AGENT-WORKSPACE.md §2.1).
  const team = here.filter((a) => a.id !== chosenSession?.agent.id && a.id !== leadAgent?.id && (!where || wsKey(a.cwd) === wsKey(where)));
  const workers = team;
  // 自动 (none picked) is a worker on the lead's login and the cheaper sibling of its model, not whoever worked last.
  const chosenWorker = worker ? workers.find((a) => a.name === worker) : undefined;
  // A chat needs only a model: the remembered one if the hub still offers it, else the first.
  const chatModel =
    chatPick && offered.some((p) => p.name === chatPick.plugin && p.models.includes(chatPick.model))
      ? chatPick
      : offered[0]
        ? { plugin: offered[0].name, model: offered[0].models[0] }
        : undefined;
  const ready =
    !busy &&
    !!request.trim() &&
    (mode === "chat" ? !!chosenSession || !!chatModel : !!lead && !!where) &&
    (!sessionUse || sessionUse.selectable);
  // Effort levels of the vendor and model picked: the chat's model, or the run's lead. A
  // model the CLI reports levels for takes exactly those; one it does not name takes the
  // plugin's list. A picked session keeps its own, so there is nothing to choose.
  const effortPlugin = plugins.data?.find((p) => p.name === (mode === "chat" ? chatModel?.plugin : leadPlugin));
  const effortModel = mode === "chat" ? chatModel?.model : leadModel;
  const levels = chosenSession
    ? []
    : effortLevels(effortPlugin?.efforts_by_model?.[effortModel ?? ""] ?? effortPlugin?.efforts);
  const effortWanted = effortPick || (mode === "run" && leadAgent ? effortOf(leadAgent.config) ?? "" : "");
  const effortOn = levels.some((l) => l.value === effortWanted) ? effortWanted : "";
  const effort = effortOn || undefined;
  const activeChoice: Choice | undefined = mode === "chat" ? chatModel : lead?.kind === "model" ? lead : leadAgent?.model ? { plugin: leadAgent.plugin, model: leadAgent.model, account: accountOf(leadAgent) } : undefined;
  const activePlugin = plugins.data?.find((p) => p.name === activeChoice?.plugin);
  const workerPlugin = plugins.data?.find((p) => p.name === (chosenWorker?.plugin ?? leadPlugin));
  const workerModel = chosenWorker?.model ?? workerModelFor(workerPlugin?.models ?? [], leadModel ?? "");
  const workerKey = JSON.stringify([chosenWorker?.id, workerPlugin?.name, workerModel]);
  const workerOverride = workerReasoning?.key === workerKey ? workerReasoning.choice : undefined;
  const workerSetting = workerOverride ?? { model: workerModel, effort: chosenWorker ? effortOf(chosenWorker.config) ?? "" : "" };
  // The choice in force: the person's pick, else the chosen agent's own (a new agent follows).
  const chatPermission: PermissionChoice = chatPerm !== undefined ? chatPerm : chosenSession ? permissionOf(chosenSession.agent) : null;
  const workerPermission: PermissionChoice = workPerm !== undefined ? workPerm : chosenWorker ? permissionOf(chosenWorker) : null;
  /** Another worker means its own permission, not the one picked for the last one. */
  const chooseWorker = (name: string) => {
    setWorker(name);
    setWorkPerm(undefined);
  };

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

  /**
   * An agent with this role, model and login in `dir`, or a new one (the hub names it: a
   * person's name, never the model's).
   *
   * `reuse` is false for the lead: 新会话 says a new one, so handing back an agent that
   * already exists — with everything it has said before — is the opposite of what was asked.
   * To carry a conversation on, pick that session by name under 已有会话. Workers are reused:
   * one is a template each task clones, not a conversation anyone reads.
   */
  async function agentFor(role: "planner" | "worker", choice: Choice, dir: string, effort?: string, reuse = true, permission?: PermissionChoice): Promise<Agent> {
    const { plugin, model, account } = choice;
    // The picked session leads this run: it cannot also be reused as the default worker.
    const found = reuse && everyone.find((a) => a.id !== chosenSession?.agent.id && a.role === role && a.plugin === plugin && a.model === model && accountOf(a) === account && wsKey(a.cwd) === wsKey(dir));
    if (found) return withEffort(found, effort);
    return api.createAgent({
      plugin,
      driver: null,
      model,
      role,
      cwd: dir,
      config: permission === undefined ? configFor(choice, effort) : configWithPermission(configFor(choice, effort), permission),
      profile: { display_name: null, title: null, avatar: null, avatar_image: null, color: null, persona: null },
    });
  }

  /**
   * A message starting with `@<model>` (a model a login offers) is a chat on that model, the
   * name taken off the message: "@hy4-preview 你好" talks to Hy4 Preview, it does not ask
   * another model to recruit it. Cursor's menu names the base model, so a bare base the exact
   * IDs do not have picks the first variant the CLI offers for it.
   */
  function leadingModel(text: string): { choice: Choice; rest: string } | undefined {
    const m = /^@(\S+)\s*/.exec(text);
    if (!m) return undefined;
    const wanted = m[1].toLowerCase();
    const pick = (plugin: string, model: string) => {
      // The login picked in the composer when it is the same vendor, else the default one.
      const account = chatModel?.plugin === plugin ? chatModel.account : undefined;
      return { choice: { plugin, model, ...(account ? { account } : {}) }, rest: text.slice(m[0].length).trim() };
    };
    for (const p of offered) {
      const model = p.models.find((x) => x.toLowerCase() === wanted);
      if (model) return pick(p.name, model);
    }
    for (const p of offered) {
      if (p.name !== "cursor") continue;
      const model = p.models.find((x) => cursorMode(x).base.toLowerCase() === wanted);
      if (model) return pick(p.name, model);
    }
    return undefined;
  }

  /** The hub's words, shown as they are. A 409 means the snapshot moved (a session got held):
   * re-read the candidates so the person can pick another one or start fresh. */
  function fail(err: unknown) {
    setError(errorText(err));
    if (err instanceof ApiError && err.status === 409) sessions.reload();
  }

  /** A chat is a new agent (named by the hub, the conversation titled by the message) and its
   * first message, or a message to a session that already exists. */
  async function startChat() {
    if (!ready) return;
    setBusy(true);
    setError(undefined);
    try {
      const typed = request.trim();
      // An existing session keeps its own model and login: just carry the conversation on.
      // Its permission is the person's to change here too; set it before the message lands.
      if (chosenSession) {
        if (chatPermission !== permissionOf(chosenSession.agent)) await applyPermission(chosenSession.agent, chatPermission);
        await api.chat(chosenSession.agent.id, chatBody(typed, chosenSession, uploads()));
        sent();
        navigate("s", chosenSession.agent.id);
        return;
      }
      if (!chatModel) return;
      const lead = leadingModel(typed);
      const choice = lead?.choice ?? chatModel;
      const text = lead?.rest || typed;
      const body = newChatRequest(choice, text, cwd ?? "", lead && lead.choice.plugin !== chatModel.plugin ? undefined : effort);
      const agent = await api.createAgent({ ...body, config: configWithPermission(body.config, chatPermission) });
      // Into the conversation at once; the message shows there while the agent starts.
      void sendMessage(agent.id, text, { attachments: uploads() }).catch(() => {});
      sent();
      navigate("s", agent.id);
    } catch (err) {
      fail(err);
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
      // A picked session leads as it is, with its conversation; otherwise the lead's effort applies.
      const planner = chosenSession ? chosenSession.agent : leadAgent ? await withEffort(leadAgent, effort) : await agentFor("planner", leadChoice, where, effort, false);
      const models = plugins.data?.find((p) => p.name === planner.plugin)?.models ?? [];
      // Workers run on the lead's login, on the cheaper sibling of its model.
      const workerChoice: Choice = { plugin: planner.plugin, model: workerModelFor(models, planner.model ?? leadModel ?? models[0] ?? ""), account: accountOf(planner) };
      const workerTemplate = chosenWorker ?? (await agentFor("worker", workerChoice, where, undefined, true, workerPermission));
      const reasoningWorker = workerOverride
        ? reasoningWorkerRequest(workerTemplate, where, workerOverride)
        : undefined;
      const workerAgent = reasoningWorker
        ? await api.createAgent({ ...reasoningWorker, config: configWithPermission(reasoningWorker.config, workerPermission) })
        : workerTemplate;
      // A worker that already existed keeps its own permission until changed here: set it
      // before the run starts, so the first task already follows the choice.
      if (workerPermission !== permissionOf(workerAgent)) await applyPermission(workerAgent, workerPermission);
      const run = await api.createRun({
        request: request.trim(),
        title: title.trim() || null,
        planner: planner.id,
        ...runSessionFields(chosenSession),
        worker: workerAgent.id,
        escalate: escalate || null,
        reviewer: reviewer || null,
        auto_approve: false,
        max_parallel: maxParallel ? Number(maxParallel) : null,
        max_attempts: null,
        team: null,
        attachments: uploads(),
        policy: {
          ...presetPolicy(preset),
          continuation: continuation ? { max_attempts: continuationAttempts, interval_seconds: continuationInterval, stuck_minutes: null } : null,
          budget: {
            max_usd: maxUsd ? Number(maxUsd) : null,
            max_minutes: maxMinutes ? Number(maxMinutes) : null,
            max_tokens: maxTokens ? Math.round(Number(maxTokens) * 1000) : null,
          },
        },
      });
      sent();
      navigate("runs", run.id);
    } catch (err) {
      fail(err);
    } finally {
      setBusy(false);
    }
  }

  /** The held files as the hub takes them. */
  function uploads() {
    return files.held.map(({ name, mime, data }) => ({ name, mime, data }));
  }

  function onKey(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.nativeEvent.isComposing) return;
    // Enter sends (Ctrl/Cmd+Enter too); Shift+Enter is a newline, as in a conversation.
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void submit();
    }
  }

  const pickFolder = window.crossagents?.pickFolder;
  /** Uses the path typed for a remote hub, as if it had come from the dialog. */
  const useManualPath = () => {
    const dir = manualPath.trim();
    if (!dir) return;
    const known = list.find((w) => w.key === wsKey(dir));
    if (known) {
      setFolder(undefined);
      selectWorkspace(known.key);
    } else setFolder(dir);
    setPathEntry(false);
  };
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
    ...(remote
      ? [
          {
            kind: "item",
            key: "path",
            label: "输入中心上的路径…",
            sub: "这个路径在中心那台机器上；建 agent 时 hub 会校验",
            icon: <Icon name="folder" size={14} />,
            onPick: () => setPathEntry(true),
          } satisfies MenuItem,
        ]
      : pickFolder
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
        ...reasoningModels(p.name, modelsFor(p, account), modelsFor(p, account).find((m) => isOn({ plugin: p.name, model: m, ...(account ? { account } : {}) }))).map((m): MenuItem => {
          const choice: Choice = { plugin: p.name, model: m, ...(account ? { account } : {}) };
          const display = modelDisplay(p.name, m, quota.data?.accounts ?? [], account);
          return { kind: "item", key: `${p.name}/${account ?? ""}/${m}`, label: p.name === "cursor" ? prettyModel(cursorMode(m).base) : display.label, sub: m, searchText: display.description ?? undefined, icon: <VendorMark plugin={p.name} size={14} />, on: isOn(choice), onPick: () => onPick(choice) };
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
        label: sessionLabel(a),
        sub: [agentTitle(a), relationText(a)].filter(Boolean).join(" · "),
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
  // A new session first, then the workspace's own; unusable ones stay listed, disabled, with
  // the reason (the hub's words for what holds a busy one).
  const sessionItems: MenuItem[] = [
    { kind: "item", key: "fresh", label: t("home.newSession"), sub: t("home.newSessionHint"), on: !chosenSession, onPick: () => setSessionId(undefined) },
    ...((sessions.data?.length ?? 0) > 0 ? [{ kind: "header", key: "h-sessions", label: t("session.existing") } satisfies MenuItem] : []),
    ...(sessions.data ?? []).map((s): MenuItem => {
      const use = usabilityOf(s);
      // The reason first: read left to right, it is what the line must not truncate away.
      const parts: string[] = [];
      if (use.reason) parts.push(t(use.reason.key, use.reason.vars));
      else if (use.hint) parts.push(t(use.hint.key, use.hint.vars));
      parts.push(s.agent.model ?? s.agent.plugin, t(sessionStateKey(s.state)));
      if (s.last_execution) parts.push(s.last_execution);
      return {
        kind: "item",
        key: `s/${s.agent.id}`,
        label: displayName(s.agent),
        sub: parts.join(" · "),
        subDir: "ltr",
        icon: <Avatar agent={s.agent} size={18} />,
        on: chosenSession?.agent.id === s.agent.id,
        disabled: !use.selectable,
        onPick: () => setSessionId(s.agent.id),
      };
    }),
  ];
  const effortItems: MenuItem[] = [
    { kind: "header", key: "h", label: t("home.effort") },
    { kind: "item", key: "", label: t("home.effortDefault"), on: !effortOn, onPick: () => setEffort("") },
    ...levels.map((l): MenuItem => ({ kind: "item", key: l.value, label: l.label, sub: l.value, on: effortOn === l.value, onPick: () => setEffort(l.value) })),
  ];
  /** "Claude Opus 5.5", plus the login when the vendor has more than one. */
  const choiceLabel = (c: Choice) => {
    const login = accountsOf(c.plugin).length > 1 ? accountLabel(c.account) ?? t("home.defaultLogin") : undefined;
    const label = c.plugin === "cursor" ? prettyModel(cursorMode(c.model).base) : modelDisplay(c.plugin, c.model, quota.data?.accounts ?? [], c.account).label;
    return `${vendorLabel(c.plugin)} ${label}${login ? ` · ${login}` : ""}`;
  };

  const wsLabel = folder ? wsName(folder) : current?.name;
  // @ names a helper: an agent already in this project, or a model to recruit one on.
  const mention = useMention(ref, setRequest, mentionsFor(cwd ? here.filter((a) => !a.config || !(a.config as Record<string, unknown>).spawned_by_agent) : [], offered));
  const missing =
    !agents.data || !plugins.data
      ? undefined
      : sessionUse && !sessionUse.selectable && sessionUse.reason
        ? t(sessionUse.reason.key, sessionUse.reason.vars)
        : !chosenSession && !offered.length
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
              <Icon name="tasks" size={14} />
              {t("home.run")}
            </button>
          </div>
        </div>

        <QuestionCards agents={everyone} />
        <div className="composer home-composer" onDrop={files.onDrop} onDragOver={files.onDragOver}>
          {mention.menu}
          <HeldFiles files={files.held} onRemove={files.remove} />
          {draftNote && <div className="small dim">{draftNote}</div>}
          <textarea
            onPaste={files.onPaste}
            ref={ref}
            rows={3}
            value={request}
            onChange={(e) => {
              setRequest(e.target.value);
              mention.sync();
            }}
            onClick={mention.sync}
            onKeyDown={(e) => mention.onKeyDown(e) || onKey(e)}
            placeholder={t(mode === "chat" ? "home.chatPlaceholder" : "home.placeholder")}
            autoFocus
          />
          {pathEntry && (
            <div className="home-path">
              <input
                data-field="cwd"
                value={manualPath}
                onChange={(e) => setManualPath(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.nativeEvent.isComposing) {
                    e.preventDefault();
                    useManualPath();
                  } else if (e.key === "Escape") setPathEntry(false);
                }}
                placeholder="中心上的路径，例如 /home/me/project 或 C:\\work\\project"
                aria-label="中心上的路径"
                autoFocus
              />
              <button type="button" className="primary small" onClick={useManualPath} disabled={!manualPath.trim()}>
                用这个路径
              </button>
              <button type="button" className="ghost small" onClick={() => setPathEntry(false)}>
                取消
              </button>
            </div>
          )}
          <div className="bar">
            <ChipMenu
              icon={wsLabel ? <span className={`ws-dot av-${hueOf(wsKey(folder ?? current!.path))}`} /> : <Icon name={mode === "chat" ? "chat" : "folder"} size={14} />}
              label={wsLabel ?? (remote ? "输入中心上的路径" : t(mode === "chat" ? "home.noProject" : "home.chooseWorkspace"))}
              title={folder ?? current?.path}
              items={wsItems}
              up
            />
            {(sessionRepo || chatsOnly || chosenSession) && (
              <ChipMenu
                icon={chosenSession ? <Avatar agent={chosenSession.agent} size={16} /> : <Icon name="chat" size={14} />}
                label={chosenSession ? `${displayName(chosenSession.agent)}${sessionUse?.restore ? ` · ${t("session.willRestore")}` : ""}` : t("home.newSession")}
                title={t("home.session")}
                items={sessionItems}
                up
              />
            )}
            {mode === "chat" && !chosenSession && chatModel && (
              <ChipMenu
                icon={<VendorMark plugin={chatModel.plugin} size={14} />}
                label={choiceLabel(chatModel)}
                title={t("home.model")}
                items={chatItems}
                searchable
                up
              />
            )}
            {mode === "run" && !chosenSession && lead && (
              <ChipMenu
                icon={leadAgent ? <Avatar agent={leadAgent} size={16} /> : <VendorMark plugin={leadPlugin!} size={14} />}
                label={leadAgent ? displayName(leadAgent) : lead.kind === "model" ? choiceLabel(lead) : ""}
                title={t("home.lead")}
                items={leadItems}
                searchable
                up
              />
            )}
            {/* After the model it applies to: the chat's, or the run lead's. */}
            {!chosenSession && activeChoice && activePlugin?.name === "cursor" && <ReasoningPicks plugin={activePlugin} model={activeChoice.model} onChange={(model) => {
              if (!model) return;
              const next = { ...activeChoice, model };
              if (mode === "chat") { setChatPick(next); save(CHAT_MODEL_KEY, next); }
              else choose({ kind: "model", ...next });
            }} />}
            {levels.length > 0 && (
              <ChipMenu
                icon={<Icon name="brain" size={13} />}
                label={`${t("home.effortShort")} ${effortOn ? effortLabel(effortOn) : t("home.effortDefault")}`}
                title={t("home.effort")}
                items={effortItems}
                up
              />
            )}
            {mode === "chat" && (
              <PermissionPicks
                variant="compact"
                label={t("home.permission")}
                value={chatPermission}
                onChange={setChatPerm}
                plugin={chosenSession?.agent.plugin ?? chatModel?.plugin}
              />
            )}
            <div className="spacer" />
            {files.picker}
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

        {missing ? (
          <p className="home-note">{missing}</p>
        ) : (
          mode === "run" && (
            <p className="home-note">
              {t(preset === "full" ? "home.whereFull" : "home.where")}
              {preset !== "manual" && (
                <button type="button" className="link" onClick={() => setOptions(true)}>
                  {" · "}
                  {PRESETS.find((p) => p.key === preset)?.label}
                </button>
              )}
              {preset === "full" && !maxUsd && !maxMinutes && !maxTokens && <span className="warn-text"> · {t("home.noCaps")}</span>}
            </p>
          )
        )}

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
                <AgentSelect agents={workers} value={chosenWorker?.name ?? ""} onChange={chooseWorker} empty={t("home.auto")} />
                {/* What the default worker may do without asking; the lead keeps its own setting. */}
                <div className="permission-line">
                  <span className="small muted">{t("home.permission")}</span>
                  <PermissionPicks
                    variant="compact"
                    label={t("home.permission")}
                    value={workerPermission}
                    onChange={setWorkPerm}
                    plugin={chosenWorker?.plugin ?? leadPlugin}
                  />
                </div>
                <div className="reasoning-picks"><ReasoningPicks plugin={workerPlugin} model={workerSetting.model} effort={workerSetting.effort} onChange={(model, effort) => setWorkerReasoning({ key: workerKey, choice: { model, effort } })} /></div>
              </Field>
              <Field label={t("home.escalate")}>
                <AgentSelect agents={workers} value={escalate} onChange={setEscalate} empty={t("home.notUsed")} />
              </Field>
              <Field label={t("home.reviewer")}>
                <AgentSelect agents={team} value={reviewer} onChange={setReviewer} empty={t("home.notUsed")} />
              </Field>
            </div>
            <div className="row row-top">
              <div className="field">
                <span>{t("home.continuation")}</span>
                <label className="field-check"><input type="checkbox" checked={continuation} onChange={(e) => setContinuation(e.target.checked)} /> {t("home.continuationWake")}</label>
                <span className="muted">{t("home.continuationHint")}</span>
              </div>
              <Field label={t("home.continuationMaxAttempts")}>
                <input type="number" min={1} max={100} disabled={!continuation} value={continuationAttempts} onChange={(e) => setContinuationAttempts(Math.max(1, Math.min(100, Number(e.target.value) || 1)))} />
              </Field>
              <Field label={t("home.continuationInterval")}>
                <input type="number" min={1} max={86400} disabled={!continuation} value={continuationInterval} onChange={(e) => setContinuationInterval(Math.max(1, Math.min(86400, Number(e.target.value) || 1)))} />
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
        {(error || sessions.error) && (
          <div style={{ marginTop: 12 }}>
            <ErrorBox error={error ?? t("session.loadFailed", { error: sessions.error ?? "" })} />
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
          {agentLine(a)}
        </option>
      ))}
    </select>
  );
}
