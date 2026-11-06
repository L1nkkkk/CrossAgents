// The run page's pieces for troubleshooting records (docs/tasks/LESSONS.md): the problems the
// run recorded and what became of each, and the records a failure may be related to.

import { useState } from "react";
import type { RelatedLesson } from "@bindings/RelatedLesson";
import { api, errorText } from "../api";
import { ErrorBox, agentName, ago, time, useAgents } from "../components";
import { useHubData, type HubEvent } from "../hub";
import { RailCard } from "../rail";
import { issueHeadline, shortId, triageLesson, triagePrompt, triageText } from "./lessons";
import "../expertise.css";
import "./lessons.css";

const issueEvent = (runId: string) => (e: HubEvent) =>
  (e.type === "issue.recorded" || e.type === "issue.triaged") && (e.data as { run_id?: string } | undefined)?.run_id === runId;

/** The run's recorded problems (run notes under issue/); a button hands the unsorted ones to the lead. */
export function RunIssues({ runId, lead }: { runId: string; lead: string }) {
  const issues = useHubData(() => api.runIssues(runId), [runId], issueEvent(runId));
  const agents = useAgents();
  const [sent, setSent] = useState<"idle" | "busy" | "done">("idle");
  const [error, setError] = useState<string>();
  const list = issues.data?.issues ?? [];
  const unsorted = issues.data?.unsorted ?? 0;
  if (!list.length && !issues.error) return null;
  const leadName = agentName(agents.data, lead);
  const handOver = async () => {
    setSent("busy");
    setError(undefined);
    try {
      await api.sendMessage(lead, { text: triagePrompt(unsorted) });
      setSent("done");
    } catch (err) {
      setError(errorText(err));
      setSent("idle");
    }
  };
  return (
    <RailCard id="run-issues" title="问题记录" aside={unsorted ? `${unsorted} 条未整理` : String(list.length)}>
      <ErrorBox error={issues.error ?? error} />
      <div className="run-issues">
        {list.map((i) => {
          const lesson = triageLesson(i.triage);
          return (
            <div key={i.key} className={`run-issue ${i.triage ? "" : "unsorted"}`}>
              <div className="run-issue-head">
                <b title={i.text}>{issueHeadline(i.text) || i.key}</b>
                <span className="muted" title={time(i.updated_at)}>
                  {agentName(agents.data, i.author)} · {ago(i.updated_at)}
                </span>
              </div>
              <div className="muted">
                {i.triage ? (
                  lesson ? (
                    <a href="#/lessons" title={lesson}>
                      {triageText(i.triage)}
                    </a>
                  ) : (
                    triageText(i.triage)
                  )
                ) : (
                  <span className="exp-tag yellow">未整理</span>
                )}
              </div>
            </div>
          );
        })}
        {unsorted > 0 && (
          <div className="run-issue-actions">
            <button onClick={handOver} disabled={sent !== "idle"}>
              {sent === "done" ? `已交给 ${leadName}` : `交给 ${leadName} 整理`}
            </button>
            <span className="muted">发一条消息给 lead，让它逐条整理成排障记录</span>
          </div>
        )}
      </div>
    </RailCard>
  );
}

/** The records a failure may be related to: possibly related, not confirmed to apply. */
export function RelatedLessons({ list }: { list: RelatedLesson[] }) {
  if (!list.length) return null;
  return (
    <div className="msg-card related-lessons">
      <p className="muted small">只是词语对上了，不代表它的处理适用这里；先看它的条件。</p>
      <ul className="lesson-sources">
        {list.map((l) => (
          <li key={l.id}>
            <code title={l.id}>{shortId(l.id)}</code>
            <a href="#/lessons">{l.title}</a>
            {l.matched.map((m) => (
              <span key={m} className="exp-tag">
                {m}
              </span>
            ))}
          </li>
        ))}
      </ul>
    </div>
  );
}
