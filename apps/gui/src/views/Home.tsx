import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { ImportableSession } from "@bindings/ImportableSession";
import type { PluginInfo } from "@bindings/PluginInfo";
import type { SessionCandidate } from "@bindings/SessionCandidate";
import { api, ApiError, errorText } from "../api";
import { HeldFiles, useHeld } from "../attach";
import { sendMessage } from "../outbox";
import { navigate } from "../App";
import { Avatar, BrandMark, ErrorBox, Field, Icon, VendorMark, agentLine, agentTitle, displayName, hueOf, relationText, sessionLabel, useAgents } from "../components";
import { useHubData } from "../hub";
import { CENTRE_MACHINE } from "../lib/sidebarTree";
import { agentMachineKey, normalizeNodeOs, selectNode, useNodePlugins, useNodes, useSelectedNode } from "../nodes";
import { OsIcon } from "../osIcon";
import { useConnectionState } from "../lib/useConnection";
import { t } from "../lib/i18n";
import { isImeEvent } from "../lib/ime";
import { modelDisplay } from "../lib/modelDisplay";
import { favoriteKey, toggleFavorite, useModelFavorites } from "../lib/modelFavorites";
import { cursorMode, reasoningModels, usesVariantReasoning } from "../lib/reasoning";
import { ReasoningPicks } from "../reasoningPicks";
import { reasoningWorkerRequest, type ReasoningChoice } from "../lib/workerModels";
import { effortLabel, effortLevels, effortOf, prettyModel, shortModelLabel, vendorLabel, workerModelFor, accountOf } from "../lib/models";
import { chatBody, newChatRequest, runSessionFields, sessionStateKey, usabilityOf } from "../lib/sessionChoice";
import { configWithPermission, permissionOf, type PermissionChoice } from "../lib/permission";
import { PermissionPicks, applyPermission } from "../permissionPicks";
import * as draft from "../lib/draft";
import { useQuota } from "./Quota";
import { PRESETS, presetPolicy, type Preset } from "../lib/policy";
import { ChipMenu, type MenuItem } from "../menu";
import { mentionsFor, sessionMentionsFor, useMention } from "../mention";
import { HeldSessionRefs, useSessionRefs } from "../sessionRef";
import { QuestionCards } from "../questions";
import { RemoteDirPicker } from "../RemoteDirPicker";
import { CHATS, homeOf, isChat, selectWorkspace, useSelectedWorkspace, useWorkspaces, wsKey, wsName } from "../workspace";

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

function shortAgo(iso: string): string {
  const diff = Math.max(0, Math.floor((Date.now() - Date.parse(iso)) / 1000));
  if (Number.isNaN(diff) || diff < 60) return "刚刚";
  const mins = Math.floor(diff / 60);
  if (mins < 60) return `${mins} 分钟前`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  return `${days} 天前`;
}

function shortImportTitle(title: string, fallbackId: string): string {
  const s = (title || fallbackId.slice(0, 8)).trim();
  return s.length > 22 ? `${s.slice(0, 22)}…` : s;
}

/**
 * Start page, quiet like Claude Code's: a greeting and one composer. Where (a workspace) and
 * who leads (a model or an agent) are chips in the composer. Picking a model is enough: the
 * page reuses or creates the agents the run needs, and the lead recruits the rest itself.
 * Everything else waits behind the options button.
 */
export function HomeView() {
  const agents = useAgents();
  const nodes = useNodes();
  const selectedNode = useSelectedNode();
  const remoteNodes = (nodes.data ?? []).filter((n) => !n.local);
  const localNode = (nodes.data ?? []).find((n) => n.local);
  const activeRemote = selectedNode
    ? remoteNodes.find((n) => n.id === selectedNode || n.name === selectedNode)
    : undefined;
  const activeNodeId = activeRemote?.id ?? "";
  const isRemoteNode = !!activeRemote;
  const plugins = useNodePlugins(activeNodeId);
  const scope = useSelectedWorkspace();
  const { list: all } = useWorkspaces();
  const targetMachineKey = activeNodeId || CENTRE_MACHINE;
  const everyoneAll = (agents.data ?? []).filter((a) => !a.archived_at);
  const machineAgents = everyoneAll.filter((a) => agentMachineKey(a) === targetMachineKey);
  // Projects scoped to the currently selected execution machine (centre or remote node).
  const list = (() => {
    const map = new Map<string, { key: string; path: string; name: string }>();
    if (!isRemoteNode) {
      for (const w of all) {
        if (w.key === CHATS) continue;
        if (w.runs.length > 0 || w.agents.some((a) => agentMachineKey(a) === CENTRE_MACHINE)) {
          map.set(w.key, { key: w.key, path: w.path, name: w.name });
        }
      }
    }
    for (const a of machineAgents) {
      if (isChat(a)) continue;
      const h = homeOf(a);
      if (!h) continue;
      const k = wsKey(h);
      if (!map.has(k)) map.set(k, { key: k, path: h, name: wsName(h) });
    }
    return [...map.values()].sort((a, b) => a.key.localeCompare(b.key));
  })();
  const current = list.find((w) => w.key === scope);
  const [folder, setFolder] = useState<string>();
  // A remote hub or a remote execution node has its folders on that machine: there is no
  // local system dialog for them, so the person types the path on that machine.
  const remote = useConnectionState()?.mode === "remote";
  const manualOnly = remote || isRemoteNode;
  const [pathEntry, setPathEntry] = useState(false);
  const [dirPickerOpen, setDirPickerOpen] = useState(false);
  const [manualPath, setManualPath] = useState("");
  const cwd = folder ?? current?.path;
  const [mode, setModeState] = useState<Mode>(() => load<Mode>(MODE_KEY, "chat", (v) => v === "chat" || v === "run"));
  const setMode = (m: Mode) => {
    if (isRemoteNode && m === "run") return;
    setModeState(m);
    save(MODE_KEY, m);
  };
  useEffect(() => {
    if (isRemoteNode && mode === "run") setModeState("chat");
  }, [isRemoteNode, mode]);
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
  const everyone = machineAgents;
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
  const sessionRefs = useSessionRefs(setError);
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
    sessionRefs.clear();
  };

  // The delegation level carries over, 全托管 included (docs/tasks/W1.md §7).
  const setPreset = (p: Preset) => {
    setPresetState(p);
    save(PRESET_KEY, p);
  };
  const choose = (lead: Lead) => {
    setSessionId(undefined);
    setImportPick(undefined);
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
  const [importAllDirs, setImportAllDirs] = useState(false);
  const [importShowAll, setImportShowAll] = useState(false);
  const [chatWorktree, setChatWorktree] = useState(false);
  const [modelOpenSignal, setModelOpenSignal] = useState(0);
  const favorites = useModelFavorites();
  const importable = useHubData(
    () =>
      isRemoteNode
        ? Promise.resolve([] as ImportableSession[])
        : api.importableSessions(importAllDirs ? undefined : sessionRepo).then((r) => r.candidates.filter((c) => !c.existing_agent_id)),
    [sessionRepo, importAllDirs, isRemoteNode],
    (e) => e.type === "agent.created" || e.type === "agent.removed",
  );
  useEffect(() => {
    const onShortcut = (e: globalThis.KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "m") {
        e.preventDefault();
        setModelOpenSignal((n) => n + 1);
      }
    };
    window.addEventListener("keydown", onShortcut);
    return () => window.removeEventListener("keydown", onShortcut);
  }, []);
  // The session picked to carry this start; absent: a new one, as before.
  const [sessionId, setSessionId] = useState<string>();
  const [importPick, setImportPick] = useState<ImportableSession>();
  useEffect(() => {
    setSessionId(undefined);
    setImportPick(undefined);
  }, [sessionRepo, chatsOnly]);
  useEffect(() => {
    if (sessionId && sessions.data && !sessions.data.some((s) => s.agent.id === sessionId)) setSessionId(undefined);
  }, [sessions.data, sessionId]);
  useEffect(() => {
    if (importPick && importable.data && !importable.data.some((c) => c.session_id === importPick.session_id)) setImportPick(undefined);
  }, [importable.data, importPick]);
  const chosenSession = sessionId ? sessions.data?.find((s) => s.agent.id === sessionId) : undefined;
  const chosenImport = importPick ? importable.data?.find((c) => c.session_id === importPick.session_id) : undefined;
  // The permission pick follows whoever carries this start: another session or worker means
  // its own setting, until the person picks one here.
  useEffect(() => setChatPerm(undefined), [chosenSession?.agent.id, chosenImport?.session_id]);
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
  const where = leadAgent?.cwd ?? cwd ?? (chosenImport?.cwd || undefined);

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
    (mode === "chat" && (chosenImport || sessionRefs.held.length > 0) ? true : !!request.trim()) &&
    (mode === "chat" ? !!chosenSession || !!chosenImport || !!chatModel : (!!lead || !!chosenImport) && !!where) &&
    (!sessionUse || sessionUse.selectable);
  // Effort levels of the vendor and model picked: the chat's model, or the run's lead. A
  // model the CLI reports levels for takes exactly those; one it does not name takes the
  // plugin's list. A picked session keeps its own, so there is nothing to choose.
  const effortPlugin = plugins.data?.find((p) => p.name === (mode === "chat" ? chatModel?.plugin : leadPlugin));
  const effortModel = mode === "chat" ? chatModel?.model : leadModel;
  const levels = chosenSession || chosenImport
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
      if (!usesVariantReasoning(p.name)) continue;
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
      // `/model <query>` switches the chat model right from the composer without sending a turn.
      const slashModel = /^\/model(?:\s+(.*))?$/i.exec(typed);
      if (slashModel) {
        const q = (slashModel[1] ?? "").trim().toLowerCase();
        if (!q) {
          setRequest("");
          setModelOpenSignal((n) => n + 1);
          return;
        }
        const hit = offered
          .flatMap((p) => p.models.map((m) => ({ plugin: p.name, model: m })))
          .find((x) => `${x.plugin} ${x.model}`.toLowerCase().includes(q));
        if (hit) {
          const account = chatModel?.plugin === hit.plugin ? chatModel.account : undefined;
          const next: Choice = { ...hit, ...(account ? { account } : {}) };
          setChatPick(next);
          save(CHAT_MODEL_KEY, next);
          setRequest("");
          return;
        }
        setError(`找不到匹配「${q}」的模型`);
        return;
      }
      // An existing session keeps its own model and login: just carry the conversation on.
      // Its permission is the person's to change here too; set it before the message lands.
      if (chosenSession) {
        if (chatPermission !== permissionOf(chosenSession.agent)) await applyPermission(chosenSession.agent, chatPermission);
        await api.chat(chosenSession.agent.id, chatBody(typed, chosenSession, uploads(), sessionRefs.toInputs()));
        sent();
        navigate("s", chosenSession.agent.id);
        return;
      }
      if (chosenImport) {
        const agent = await api.importSession({
          plugin: chosenImport.plugin,
          session_id: chosenImport.session_id,
          cwd: cwd ?? (chosenImport.cwd || undefined),
          account: chosenImport.account,
          permission: chatPermission ?? undefined,
        });
        if (typed || uploads().length > 0 || sessionRefs.held.length > 0) {
          void sendMessage(agent.id, typed, { attachments: uploads(), sessionRefs: sessionRefs.held }).catch(() => {});
        }
        setImportPick(undefined);
        sent();
        navigate("s", agent.id);
        return;
      }
      if (!chatModel) return;
      const lead = leadingModel(typed);
      const choice = lead?.choice ?? chatModel;
      const text = lead?.rest || typed;
      const body = newChatRequest(choice, text, cwd ?? "", lead && lead.choice.plugin !== chatModel.plugin ? undefined : effort);
      const agent = await api.createAgent({
        ...body,
        config: configWithPermission(body.config, chatPermission),
        ...(!isRemoteNode && chatWorktree && cwd ? { worktree: true } : {}),
        ...(isRemoteNode ? { node: activeRemote.id } : {}),
      });
      // Into the conversation at once; the message shows there while the agent starts.
      void sendMessage(agent.id, text, { attachments: uploads(), sessionRefs: sessionRefs.held }).catch(() => {});
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
    if (!ready || (!lead && !chosenImport) || !where) return;
    setBusy(true);
    setError(undefined);
    try {
      const importedLead = chosenImport
        ? await api.importSession({
            plugin: chosenImport.plugin,
            session_id: chosenImport.session_id,
            cwd: where,
            account: chosenImport.account,
          })
        : undefined;
      if (importedLead) setImportPick(undefined);
      const leadChoice: Choice = importedLead
        ? { plugin: importedLead.plugin, model: importedLead.model ?? "", account: accountOf(importedLead) }
        : lead!.kind === "model"
          ? lead!
          : { plugin: leadPlugin!, model: leadModel!, account: leadAgent && accountOf(leadAgent) };
      // A picked session leads as it is, with its conversation; otherwise the lead's effort applies.
      const planner = importedLead ?? (chosenSession ? chosenSession.agent : leadAgent ? await withEffort(leadAgent, effort) : await agentFor("planner", leadChoice, where, effort, false));
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
    if (isImeEvent(e)) return;
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
  const pickRemoteDir = (dir: string) => {
    setDirPickerOpen(false);
    setPathEntry(false);
    setManualPath("");
    const known = list.find((w) => w.key === wsKey(dir));
    if (known) {
      setFolder(undefined);
      selectWorkspace(known.key);
    } else {
      setFolder(dir);
    }
  };
  const centreLabel = localNode?.hostname || "中心";
  const browseTargetNode = isRemoteNode ? activeRemote.id : "local";
  const browseTargetLabel = isRemoteNode ? activeRemote.name : centreLabel;
  const browseTargetOs = isRemoteNode ? activeRemote.os : localNode?.os;
  const browseInitialPath = folder ?? current?.path ?? (isRemoteNode ? activeRemote.default_cwd : "");

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
    ...(manualOnly
      ? [
          {
            kind: "item",
            key: "browse",
            label: `浏览 ${browseTargetLabel} 目录…`,
            sub: `直接查看并选择 ${browseTargetLabel} 上的文件夹`,
            icon: <Icon name="folder" size={14} />,
            onPick: () => setDirPickerOpen(true),
          } satisfies MenuItem,
          {
            kind: "item",
            key: "path",
            label: isRemoteNode ? `手动输入 ${activeRemote.name} 路径…` : "手动输入中心路径…",
            sub: isRemoteNode
              ? `这个路径在 ${activeRemote.name}（${activeRemote.os}）上`
              : "这个路径在中心那台机器上；建 agent 时 hub 会校验",
            icon: <Icon name="folder" size={14} />,
            onPick: () => {
              if (!manualPath && isRemoteNode && activeRemote.default_cwd) setManualPath(activeRemote.default_cwd);
              setPathEntry(true);
            },
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

  const switchTargetNode = (nextNodeId: string) => {
    selectNode(nextNodeId);
    setFolder(undefined);
    selectWorkspace("");
    setSessionId(undefined);
    setImportPick(undefined);
    setPathEntry(false);
    setDirPickerOpen(false);
    setManualPath("");
  };
  const nodeItems: MenuItem[] =
    remoteNodes.length > 0
      ? [
          {
            kind: "item",
            key: "node:local",
            label: centreLabel,
            sub: `中心 · 在线${localNode?.plugins?.filter((p) => p.installed).length ? ` · ${localNode.plugins.filter((p) => p.installed).map((p) => p.name).join(" / ")}` : ""}`,
            icon: <OsIcon os={normalizeNodeOs(localNode?.os)} size={14} />,
            on: !isRemoteNode,
            onPick: () => switchTargetNode(""),
          },
          ...remoteNodes.map((n): MenuItem => {
            const installed = (n.plugins ?? []).filter((p) => p.installed).map((p) => p.name);
            return {
              kind: "item",
              key: `node:${n.id}`,
              label: n.name,
              sub: n.online
                ? `在线${installed.length ? ` · ${installed.join(" / ")}` : ` · ${n.os}`}`
                : "离线（请在该机器运行 crossagents node run）",
              icon: <OsIcon os={normalizeNodeOs(n.os)} size={14} />,
              on: activeNodeId === n.id,
              disabled: !n.online,
              onPick: () => switchTargetNode(n.id),
            };
          }),
        ]
      : [];

  /**
   * Models by vendor; a vendor with several logins gets a group per login, so picking a model
   * also picks who pays for it.
   */
  const modelMenu = (isOn: (c: Choice) => boolean, onPick: (c: Choice) => void): MenuItem[] => {
    const favItems: MenuItem[] = [];
    const grouped = offered.flatMap((p: PluginInfo): MenuItem[] => {
      const logins = accountsOf(p.name);
      const groups = logins.length > 1 ? logins.map((a) => ({ account: a.account.id, title: `${vendorLabel(p.name)} · ${a.account.label || a.account.id}` })) : [{ account: undefined, title: vendorLabel(p.name) }];
      return groups.flatMap(({ account, title }): MenuItem[] => [
        { kind: "header", key: `h-${p.name}-${account ?? ""}`, label: title },
        ...reasoningModels(p.name, modelsFor(p, account), modelsFor(p, account).find((m) => isOn({ plugin: p.name, model: m, ...(account ? { account } : {}) }))).map((m): MenuItem => {
          const choice: Choice = { plugin: p.name, model: m, ...(account ? { account } : {}) };
          const display = modelDisplay(p.name, m, quota.data?.accounts ?? [], account);
          const fkey = favoriteKey(p.name, m, account);
          const starred = favorites.has(fkey);
          const item: MenuItem = {
            kind: "item",
            key: `${p.name}/${account ?? ""}/${m}`,
            label: usesVariantReasoning(p.name) ? prettyModel(cursorMode(m).base) : display.label,
            sub: m,
            searchText: [vendorLabel(p.name), display.description].filter(Boolean).join(" "),
            icon: <VendorMark plugin={p.name} size={14} />,
            on: isOn(choice),
            starred,
            onToggleStar: () => toggleFavorite(fkey),
            onPick: () => onPick(choice),
          };
          if (starred) favItems.push({ ...item, key: `fav/${item.key}` });
          return item;
        }),
      ]);
    });
    return favItems.length > 0 ? [{ kind: "header", key: "h-fav", label: "常用模型" }, ...favItems, ...grouped] : grouped;
  };
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
  const importList = importable.data ?? [];
  const IMPORT_CAP = 5;
  const shownImports = importShowAll ? importList : importList.slice(0, IMPORT_CAP);
  const sessionItems: MenuItem[] = [
    {
      kind: "item",
      key: "fresh",
      label: t("home.newSession"),
      sub: t("home.newSessionHint"),
      on: !chosenSession && !chosenImport,
      onPick: () => {
        setSessionId(undefined);
        setImportPick(undefined);
      },
    },
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
        searchText: [s.agent.topic, s.agent.cwd, s.agent.model, s.agent.plugin].filter(Boolean).join(" "),
        subDir: "ltr",
        icon: <Avatar agent={s.agent} size={18} />,
        on: chosenSession?.agent.id === s.agent.id,
        disabled: !use.selectable,
        onPick: () => {
          setSessionId(s.agent.id);
          setImportPick(undefined);
        },
      };
    }),
    ...(importList.length > 0 || sessionRepo
      ? [{ kind: "header", key: "h-import", label: importAllDirs ? `${t("session.localCodex")}（全部目录）` : t("session.localCodex") } satisfies MenuItem]
      : []),
    ...shownImports.map((c): MenuItem => ({
      kind: "item",
      key: `imp/${c.session_id}`,
      label: c.title || c.session_id.slice(0, 8),
      sub: [
        c.model ? shortModelLabel(c.model) : "Codex",
        t("session.importTurns", { n: c.turns }),
        shortAgo(c.updated_at),
        (!sessionRepo || importAllDirs) && c.cwd ? wsName(c.cwd) : undefined,
      ]
        .filter(Boolean)
        .join(" · "),
      searchText: [c.title, c.cwd, c.model, c.session_id, "codex"].filter(Boolean).join(" "),
      subDir: "ltr",
      icon: <VendorMark plugin={c.plugin} size={16} />,
      on: chosenImport?.session_id === c.session_id,
      onPick: () => {
        setImportPick(c);
        setSessionId(undefined);
      },
    })),
    ...(importList.length > IMPORT_CAP
      ? [
          {
            kind: "item",
            key: "imp-more",
            label: importShowAll ? "收起本地会话" : `显示全部 ${importList.length} 条本地 Codex 会话`,
            keepOpen: true,
            onPick: () => setImportShowAll((v) => !v),
          } satisfies MenuItem,
        ]
      : []),
    ...(sessionRepo
      ? [
          {
            kind: "item",
            key: "imp-scope",
            label: importAllDirs ? "仅看当前项目的 Codex 会话" : "查看所有目录的 Codex 会话",
            keepOpen: true,
            onPick: () => {
              setImportAllDirs((v) => !v);
              setImportShowAll(false);
            },
          } satisfies MenuItem,
        ]
      : []),
  ];
  const effortFooter =
    levels.length > 0 ? (
      <>
        <span className="menu-footer-label">{t("home.effort")}</span>
        <div className="effort-pills">
          <button type="button" className={`effort-pill ${!effortOn ? "on" : ""}`} onClick={() => setEffort("")}>
            {t("home.effortDefault")}
          </button>
          {levels.map((l) => (
            <button
              key={l.value}
              type="button"
              className={`effort-pill ${effortOn === l.value ? "on" : ""}`}
              onClick={() => setEffort(l.value)}
            >
              {l.label}
            </button>
          ))}
        </div>
      </>
    ) : undefined;
  /** "Claude Opus 5.5", plus the login when the vendor has more than one. */
  const choiceLabel = (c: Choice) => {
    const login = accountsOf(c.plugin).length > 1 ? accountLabel(c.account) ?? t("home.defaultLogin") : undefined;
    const label = usesVariantReasoning(c.plugin) ? prettyModel(cursorMode(c.model).base) : modelDisplay(c.plugin, c.model, quota.data?.accounts ?? [], c.account).label;
    return `${vendorLabel(c.plugin)} ${label}${login ? ` · ${login}` : ""}`;
  };
  /**
   * What a chip writes: only the model's own name, without the vendor and without a gateway's
   * word (`DeepSeek V4.1 Flash`, not `With Tokenhub Deepseek V4.1 Flash`). The mark already names
   * the vendor, and a long account name here is what pushed the other chips onto a second line.
   * The whole name stays in the chip's tooltip.
   */
  const chipLabel = (c: Choice) => {
    const login = accountsOf(c.plugin).length > 1 ? accountLabel(c.account) ?? t("home.defaultLogin") : undefined;
    const label = usesVariantReasoning(c.plugin) ? prettyModel(cursorMode(c.model).base) : modelDisplay(c.plugin, c.model, quota.data?.accounts ?? [], c.account).label;
    return `${shortModelLabel(label)}${login ? ` · ${login}` : ""}`;
  };

  const wsLabel = folder ? wsName(folder) : current?.name;
  // @ references a past session or names a helper / model to recruit one on.
  const mention = useMention(ref, setRequest, [
    ...(mode === "chat" ? sessionMentionsFor(agents.data ?? [], sessionRefs.add) : []),
    ...mentionsFor(cwd ? here.filter((a) => !a.config || !(a.config as Record<string, unknown>).spawned_by_agent) : [], offered),
  ]);
  const missing =
    !agents.data || !plugins.data
      ? undefined
      : isRemoteNode && !activeRemote.online
        ? `节点「${activeRemote.name}」当前离线，请在该机器上运行 crossagents node run`
        : sessionUse && !sessionUse.selectable && sessionUse.reason
          ? t(sessionUse.reason.key, sessionUse.reason.vars)
          : !chosenSession && !chosenImport && !offered.length
            ? isRemoteNode
              ? `节点「${activeRemote.name}」上没有检测到可用的 CLI 模型`
              : t("home.noModels")
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
            <button
              className={mode === "run" ? "active" : ""}
              role="radio"
              aria-checked={mode === "run"}
              disabled={isRemoteNode}
              onClick={() => setMode("run")}
              title={isRemoteNode ? "远程节点目前支持 Chat 模式；Work 请切换回中心机器" : t("home.runHint")}
            >
              <Icon name="tasks" size={14} />
              {t("home.run")}
            </button>
          </div>
        </div>

        <QuestionCards agents={everyone} />
        <div
          className={`composer home-composer ${sessionRefs.dragOver ? "drop-session-over" : ""}`}
          onDrop={(e) => {
            if (!sessionRefs.onDrop(e)) files.onDrop(e);
          }}
          onDragOver={(e) => {
            if (mode === "chat") sessionRefs.onDragOver(e);
            files.onDragOver(e);
          }}
          onDragLeave={sessionRefs.onDragLeave}
        >
          {sessionRefs.dragOver && <div className="composer-drop-hint">松手引用该会话（只读上下文）</div>}
          {mention.menu}
          <HeldSessionRefs refs={sessionRefs.held} onRemove={sessionRefs.remove} onCycleMode={sessionRefs.cycleMode} />
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
                  if (e.key === "Enter" && !isImeEvent(e)) {
                    e.preventDefault();
                    useManualPath();
                  } else if (e.key === "Escape") setPathEntry(false);
                }}
                placeholder={
                  isRemoteNode
                    ? `${activeRemote.name} 上的路径，例如 ${activeRemote.default_cwd || "/Users/you/project"}`
                    : "中心上的路径，例如 /home/me/project 或 C:\\work\\project"
                }
                aria-label={isRemoteNode ? `${activeRemote.name} 上的路径` : "中心上的路径"}
                autoFocus
              />
              <button type="button" className="primary small" onClick={useManualPath} disabled={!manualPath.trim()}>
                用这个路径
              </button>
              <button type="button" className="ghost small" onClick={() => setDirPickerOpen(true)}>
                浏览目录
              </button>
              <button type="button" className="ghost small" onClick={() => setPathEntry(false)}>
                取消
              </button>
            </div>
          )}
          {dirPickerOpen && (
            <RemoteDirPicker
              node={browseTargetNode}
              label={browseTargetLabel}
              os={browseTargetOs}
              initialPath={browseInitialPath}
              onSelect={pickRemoteDir}
              onClose={() => setDirPickerOpen(false)}
            />
          )}
          {/* Two parts that keep their place in both modes: the chips a start is set up with
              wrap among themselves on the left, the actions stay pinned to the bottom right.
              As one wrapping row with a spacer, the send button dropped into the middle of a
              second row as soon as the chips needed one, so Chat and Work looked different. */}
          <div className="bar">
            <div className="bar-chips">
              {nodeItems.length > 0 && (
                <ChipMenu
                  icon={<OsIcon os={normalizeNodeOs(isRemoteNode ? activeRemote.os : localNode?.os)} size={14} />}
                  label={isRemoteNode ? activeRemote.name : centreLabel}
                  title={`执行机器：${isRemoteNode ? `${activeRemote.name}（${activeRemote.os}）` : `${centreLabel}（中心）`}`}
                  items={nodeItems}
                  up
                />
              )}
              <ChipMenu
                icon={wsLabel ? <span className={`ws-dot av-${hueOf(wsKey(folder ?? current!.path))}`} /> : <Icon name={mode === "chat" ? "chat" : "folder"} size={14} />}
                label={wsLabel ?? (isRemoteNode ? `${activeRemote.name} · 随便聊聊` : remote ? "输入中心上的路径" : t(mode === "chat" ? "home.noProject" : "home.chooseWorkspace"))}
                title={folder ?? current?.path}
                items={wsItems}
                up
              />
              {(sessionRepo || chatsOnly || chosenSession || chosenImport || (importable.data?.length ?? 0) > 0) && (
                <ChipMenu
                  icon={
                    chosenImport ? (
                      <VendorMark plugin={chosenImport.plugin} size={16} />
                    ) : chosenSession ? (
                      <Avatar agent={chosenSession.agent} size={16} />
                    ) : (
                      <Icon name="chat" size={14} />
                    )
                  }
                  label={
                    chosenImport
                      ? `${shortImportTitle(chosenImport.title, chosenImport.session_id)} · ${t("session.importTag")}`
                      : chosenSession
                        ? `${displayName(chosenSession.agent)}${sessionUse?.restore ? ` · ${t("session.willRestore")}` : ""}`
                        : t("home.newSession")
                  }
                  title={t("home.session")}
                  items={sessionItems}
                  searchable
                  searchPlaceholder="搜索会话、标题、目录、模型…"
                  emptyText="没有匹配的会话"
                  up
                />
              )}
              {mode === "chat" && !chosenSession && !chosenImport && chatModel && (
                <ChipMenu
                  className="chip-model"
                  icon={<VendorMark plugin={chatModel.plugin} size={14} />}
                  label={effortOn ? `${chipLabel(chatModel)} · ${effortLabel(effortOn)}` : chipLabel(chatModel)}
                  title={`${t("home.model")} · ${choiceLabel(chatModel)}${effortOn ? ` · ${effortLabel(effortOn)}` : ""}（Ctrl+Shift+M）`}
                  items={chatItems}
                  searchable
                  footer={effortFooter}
                  openSignal={modelOpenSignal}
                  up
                />
              )}
              {mode === "run" && !chosenSession && !chosenImport && lead && (
                <ChipMenu
                  className="chip-model"
                  icon={leadAgent ? <Avatar agent={leadAgent} size={16} /> : <VendorMark plugin={leadPlugin!} size={14} />}
                  label={leadAgent ? displayName(leadAgent) : lead.kind === "model" ? (effortOn ? `${chipLabel(lead)} · ${effortLabel(effortOn)}` : chipLabel(lead)) : ""}
                  title={leadAgent ? t("home.lead") : lead.kind === "model" ? `${t("home.lead")} · ${choiceLabel(lead)}（Ctrl+Shift+M）` : t("home.lead")}
                  items={leadItems}
                  searchable
                  footer={effortFooter}
                  openSignal={modelOpenSignal}
                  up
                />
              )}
              {/* After the model it applies to: the chat's, or the run lead's. */}
              {!chosenSession && !chosenImport && activeChoice && usesVariantReasoning(activePlugin?.name) && (
                <ReasoningPicks
                  plugin={activePlugin}
                  model={activeChoice.model}
                  variantsOnly
                  onChange={(model) => {
                    if (!model) return;
                    const next = { ...activeChoice, model };
                    if (mode === "chat") {
                      setChatPick(next);
                      save(CHAT_MODEL_KEY, next);
                    } else choose({ kind: "model", ...next });
                  }}
                />
              )}
            </div>
            <div className="bar-actions">
              {files.picker}
              {/* Both modes set their options behind this button: a chat's permission, a run's
                  worker, budget and continuation. With the chat's permission on the bar itself,
                  Chat carried one chip more than Work and wrapped a row earlier. */}
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
        </div>

        {/* The line under the box keeps its place in both modes: turning Chat into Work must
            not move the greeting above it. */}
        <div className="home-note">
          {missing ? (
            <span>{missing}</span>
          ) : chosenImport && mode === "chat" && !request.trim() ? (
            <span>{t("session.importHint")}</span>
          ) : (
            mode === "run" && (
              <span>
                {t(preset === "full" ? "home.whereFull" : "home.where")}
                {preset !== "manual" && (
                  <button type="button" className="link" onClick={() => setOptions(true)}>
                    {" · "}
                    {PRESETS.find((p) => p.key === preset)?.label}
                  </button>
                )}
                {preset === "full" && !maxUsd && !maxMinutes && !maxTokens && <span className="warn-text"> · {t("home.noCaps")}</span>}
              </span>
            )
          )}
        </div>

        {options && (mode === "chat" ? (
          // A chat's options: how its agent may act. The pick is the chat's own when one is
          // chosen, else the agent a new chat would create.
          <div className="card form home-options">
            <Field label={t("home.permission")}>
              <PermissionPicks
                label={t("home.permission")}
                value={chatPermission}
                onChange={setChatPerm}
                plugin={chosenImport?.plugin ?? chosenSession?.agent.plugin ?? chatModel?.plugin}
              />
            </Field>
            {!isRemoteNode && cwd && !chosenSession && !chosenImport && (
              <div className="field">
                <span>分支隔离</span>
                <label className="field-check">
                  <input type="checkbox" checked={chatWorktree} onChange={(e) => setChatWorktree(e.target.checked)} />
                  在独立 Git worktree 中开启（不改动当前分支）
                </label>
              </div>
            )}
          </div>
        ) : (
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
        ))}
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
