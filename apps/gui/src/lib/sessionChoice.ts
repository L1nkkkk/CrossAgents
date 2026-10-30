// Choosing who carries a new Work or Chat on: a fresh session (the old behaviour) or one that
// already lives in the workspace. Pure, so the request bodies and the reasons a candidate is
// refused are tested without a page (sessionChoice.test.ts).

import type { CreateAgentRequest } from "@bindings/CreateAgentRequest";
import type { CreateRunRequest } from "@bindings/CreateRunRequest";
import type { SessionCandidate } from "@bindings/SessionCandidate";
import type { AttachmentUpload } from "@bindings/AttachmentUpload";
import type { StartChatRequest } from "@bindings/StartChatRequest";
import type { Key } from "./i18n";
import { chatTitle } from "./models.ts";

/** A sentence for the person: a dictionary key and what fills its placeholders. */
export type SessionReason = { key: Key; vars?: Record<string, string | number> };

export type SessionUsability = {
  /** False: the entry is listed, disabled, with `reason`. */
  selectable: boolean;
  /** True: using it asks the hub to restore it first (`restore: true`). */
  restore: boolean;
  /** Shown beside a selectable entry that needs a step first (archived). */
  hint?: SessionReason;
  /** Why it cannot be picked now. */
  reason?: SessionReason;
};

const STATE_KEYS: Record<string, Key> = {
  idle: "session.state.idle",
  busy: "session.state.busy",
  archived: "session.state.archived",
};

/** The dictionary key for a candidate's state; unknown states say so. */
export function sessionStateKey(state: string): Key {
  return STATE_KEYS[state] ?? "session.state.other";
}

/**
 * Whether a candidate can be picked, and what to tell the person. `can_use` is the hub's
 * answer and the only thing that disables an entry; `reason` only explains it. That matters
 * for an archived session: it is `can_use: true` with `reason: "archived"`, and picking it
 * sends `restore: true` — disabling every non-null reason would make restoring unreachable.
 * The hub's snapshot is advisory (chat does not claim), so a refusal at send time keeps the
 * hub's own words.
 */
export function usabilityOf(candidate: SessionCandidate): SessionUsability {
  if (!candidate.can_use) {
    if (candidate.reason === "busy") {
      // The hub's sentence already says who and which run; show it as it is.
      return {
        selectable: false,
        restore: false,
        reason: candidate.busy_with
          ? { key: "session.reason.busy", vars: { what: candidate.busy_with } }
          : { key: "session.reason.busyFallback" },
      };
    }
    if (candidate.reason === "cannot_move") {
      return { selectable: false, restore: false, reason: { key: "session.reason.cannotMove" } };
    }
    return {
      selectable: false,
      restore: false,
      reason: { key: "session.reason.unavailable", vars: { reason: candidate.reason ?? "?" } },
    };
  }
  if (candidate.reason === "archived") {
    return { selectable: true, restore: true, hint: { key: "session.hint.archived" } };
  }
  return { selectable: true, restore: false };
}

/** What a Work asks the hub besides the usual body: nothing for a new session (the old shape). */
export type RunSessionFields = Pick<CreateRunRequest, "fresh" | "restore">;

/**
 * The `fresh` / `restore` pair for a Work. No session: `{}`, so the request is byte for byte
 * what it was. An existing one: `fresh: false`; an archived one also `restore: true`.
 */
export function runSessionFields(session: SessionCandidate | undefined): RunSessionFields {
  if (!session) return {};
  return { fresh: false, ...(session.reason === "archived" ? { restore: true } : {}) };
}

/** The body of `POST /v1/agents/{key}/chat`; an archived session needs `restore: true`. */
export function chatBody(text: string, session: SessionCandidate | undefined, attachments: AttachmentUpload[] = []): StartChatRequest {
  const body: StartChatRequest = { text, restore: session?.reason === "archived" };
  // Without files the body is as before, byte for byte.
  return attachments.length ? { ...body, attachments } : body;
}

/** A model to start a brand-new chat on, and which login of its vendor pays for it. */
export type ChatChoice = { plugin: string; model: string; account?: string };

/**
 * The agent a new chat is made as (chat mode, no existing session): titled by its first
 * message, with its login and — unless it is one typed with `@` for another vendor — the
 * composer's effort.
 */
export function newChatRequest(choice: ChatChoice, text: string, cwd: string, effort?: string): CreateAgentRequest {
  return {
    topic: chatTitle(text, 60),
    plugin: choice.plugin,
    driver: null,
    model: choice.model,
    role: "worker",
    cwd,
    config: { ...(choice.account ? { account: choice.account } : {}), ...(effort ? { effort } : {}) },
    profile: { display_name: null, title: null, avatar: null, color: null, persona: null },
  };
}
