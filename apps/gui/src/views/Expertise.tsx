import { useMemo, useState } from "react";
import { api } from "../api";
import { ErrorBox, Icon } from "../components";
import { useHubData } from "../hub";
import {
  entryFlags,
  estimateTokens,
  exportMarkdown,
  groupProfile,
  judgmentCopy,
  KIND_LABEL,
  missingVerdicts,
  ORIGIN_LABEL,
  PERSON_OWNER_ID,
  VERDICT_LABEL,
  type Candidate,
  type ConflictDecision,
  type ExpertiseCard,
  type ExpertiseEntry,
  type OwnerKind,
  type StaleHint,
  type Verdict,
} from "../lib/expertise";
import { useSelectedWorkspace, wsName } from "../workspace";
import "../expertise.css";

const VERDICTS: Verdict[] = ["keep_old", "replace", "merge", "both"];
const relevant = (e: { type: string }) => e.type === "task.progress" || e.type === "task.status_changed";
const pct = (r: number | null) => (r === null ? "—" : `${Math.round(r * 100)}%`);

/** The expertise card: first-try rate, its change, counts. */
function Card({ card }: { card: ExpertiseCard }) {
  const delta = card.delta_points;
  return (
    <div className="exp-card">
      <div>
        <span>一次合并率</span>
        <strong>{pct(card.first_try_rate)}</strong>
        {delta !== null && (
          <em className={delta > 0 ? "up" : delta < 0 ? "down" : ""}>
            {delta > 0 ? "+" : ""}
            {delta} 个百分点 较 30 天前
          </em>
        )}
        <small>
          样本 {card.sample}
          {card.small_sample && <b className="exp-tag yellow">样本少</b>}
        </small>
      </div>
      <div>
        <span>条目</span>
        <strong>{card.entries}</strong>
        <small>
          判断 {card.judgments} · 范例 {card.examples} · 边界 {card.limits}
        </small>
      </div>
      <div>
        <span>使用结果</span>
        <strong>{card.merged_first_try + card.reworked + card.dropped}</strong>
        <small>
          一次合并 {card.merged_first_try} · 返工 {card.reworked} · 丢弃 {card.dropped}
        </small>
      </div>
      <div>
        <span>需要留意</span>
        <strong>
          <i className="exp-count yellow">{card.stale + card.review}</i>
          <i className="exp-count red">{card.over_budget_domains}</i>
        </strong>
        <small>
          黄：待确认 {card.stale} · 需复核 {card.review}；红：超预算的领域 {card.over_budget_domains}
        </small>
      </div>
    </div>
  );
}

function EntryRow({ e, hints }: { e: ExpertiseEntry; hints: StaleHint[] }) {
  const [open, setOpen] = useState(false);
  const flags = entryFlags(e, hints);
  const level = flags.some((f) => f.level === "red") ? "red" : flags.length ? "yellow" : "";
  return (
    <li className={`exp-entry ${level} ${e.status === "superseded" ? "superseded" : ""}`}>
      <div className="exp-entry-head">
        <b>{e.title}</b>
        {e.status === "superseded" && <span className="exp-tag">已被替换</span>}
        {flags.map((f, i) => (
          <span key={i} className={`exp-tag ${f.level}`}>
            {f.reason}
          </span>
        ))}
        <span className="spacer" />
        <span className="muted exp-stats" title="用到 / 一次合并 / 返工 / 丢弃">
          用 {e.stats.used} · 一次合并 {e.stats.merged_first_try} · 返工 {e.stats.reworked} · 丢弃 {e.stats.dropped}
        </span>
      </div>
      {e.text && (
        <p className={open ? "open" : ""} title={open ? "点击收起" : "点击展开"} onClick={() => setOpen(!open)}>
          {e.text}
        </p>
      )}
      {e.example && (
        <dl className="exp-example">
          <dt>情况</dt>
          <dd>{e.example.situation}</dd>
          <dt>否掉</dt>
          <dd>
            {e.example.rejected}
            <span className="muted">（{e.example.rejected_why}）</span>
          </dd>
          <dt>采用</dt>
          <dd>{e.example.chosen}</dd>
        </dl>
      )}
      {e.applies_when && (
        <div className="muted exp-when">
          适用：<code>{e.applies_when}</code>
        </div>
      )}
    </li>
  );
}

function Profile({ entries, hints }: { entries: ExpertiseEntry[]; hints: StaleHint[] }) {
  const groups = groupProfile(entries);
  if (!groups.length) return <div className="empty">还没有条目。交活、返工和评审会生成候选，接受后出现在这里。</div>;
  return (
    <div className="exp-groups">
      {groups.map((g) => (
        <section key={g.domain} className="exp-group">
          <div className="exp-group-head">
            <h3>{g.domain}</h3>
            {g.budget && (
              <span className={`exp-budget ${g.budget.over ? "over" : ""}`} title="判断的 token 数 / 预算">
                {g.budget.judgment_tokens} / {g.budget.budget} token
                {g.budget.over && `（超出 ${g.budget.over_by}）`}
              </span>
            )}
          </div>
          {g.kinds.map(({ kind, entries }) => (
            <div key={kind} className="exp-kind">
              <h4>
                {KIND_LABEL[kind]} <span className="muted">{entries.length}</span>
              </h4>
              {entries.length ? (
                <ul>
                  {entries.map((e) => (
                    <EntryRow key={e.id} e={e} hints={hints} />
                  ))}
                </ul>
              ) : (
                <div className="muted exp-none">无</div>
              )}
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

function Side({ e, label }: { e: ExpertiseEntry; label: string }) {
  return (
    <div className="exp-side">
      <span className="muted">{label}</span>
      <b>{e.title}</b>
      {e.text && <p>{e.text}</p>}
      {e.kind === "judgment" && <small className="muted">{estimateTokens(judgmentCopy(e))} token</small>}
    </div>
  );
}

function Candidates({ list, entries, onDone }: { list: Candidate[]; entries: ExpertiseEntry[]; onDone: () => void }) {
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [verdicts, setVerdicts] = useState<Record<string, Record<string, Verdict>>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const decisions = (c: Candidate): ConflictDecision[] =>
    Object.entries(verdicts[c.id] ?? {}).map(([entry_id, verdict]) => ({ entry_id, verdict }));
  const selected = list.filter((c) => picked.has(c.id));
  const undecided = selected.filter((c) => missingVerdicts(c, decisions(c)).length > 0);
  const toggle = (id: string) =>
    setPicked((p) => {
      const n = new Set(p);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  const run = async (accept: boolean) => {
    setBusy(true);
    setError(undefined);
    try {
      for (const c of selected) {
        if (accept) await api.acceptCandidate(c.id, decisions(c));
        else await api.rejectCandidate(c.id);
      }
      setPicked(new Set());
      onDone();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  if (!list.length) return <div className="empty">没有待处理的候选。</div>;
  return (
    <>
      <div className="exp-batch">
        <label>
          <input
            type="checkbox"
            checked={selected.length === list.length}
            onChange={(e) => setPicked(new Set(e.target.checked ? list.map((c) => c.id) : []))}
          />
          全选
        </label>
        <span className="muted">已选 {selected.length}</span>
        <span className="spacer" />
        {undecided.length > 0 && <span className="exp-tag yellow">还有 {undecided.length} 个候选的冲突没选裁决</span>}
        <button disabled={busy || !selected.length} onClick={() => run(false)}>
          拒绝所选
        </button>
        <button className="primary" disabled={busy || !selected.length || undecided.length > 0} onClick={() => run(true)}>
          接受所选
        </button>
      </div>
      <ErrorBox error={error} />
      <ul className="exp-candidates">
        {list.map((c) => {
          const missing = missingVerdicts(c, decisions(c));
          return (
            <li key={c.id} className={picked.has(c.id) ? "picked" : ""}>
              <div className="exp-entry-head">
                <input type="checkbox" aria-label={`选择 ${c.proposed.title}`} checked={picked.has(c.id)} onChange={() => toggle(c.id)} />
                <b>{c.proposed.title}</b>
                <span className="exp-tag">{c.domain}</span>
                <span className="exp-tag">{KIND_LABEL[c.proposed.kind]}</span>
                <span className="exp-tag">来自{ORIGIN_LABEL[c.origin]}</span>
                {c.suggested_by_lead && <span className="exp-tag accent">lead 建议</span>}
                {c.conflicts_with.length > 0 && <span className="exp-tag red">冲突 {c.conflicts_with.length}</span>}
              </div>
              {c.note && <p className="exp-note">{c.note}</p>}
              {c.conflicts_with.length === 0
                ? c.proposed.text && <p>{c.proposed.text}</p>
                : c.conflicts_with.map((id) => {
                    const old = entries.find((e) => e.id === id);
                    const v = verdicts[c.id]?.[id];
                    return (
                      <div key={id} className={`exp-conflict ${!v ? "undecided" : ""}`}>
                        <div className="exp-pair">
                          {old ? <Side e={old} label="旧条目" /> : <div className="exp-side muted">旧条目 {id}（不在当前档案里）</div>}
                          <Side e={c.proposed} label="新条目" />
                        </div>
                        <div className="exp-verdict">
                          <span className="muted">裁决</span>
                          <div className="segmented small" role="radiogroup" aria-label="裁决">
                            {VERDICTS.map((x) => (
                              <button
                                key={x}
                                role="radio"
                                aria-checked={v === x}
                                className={v === x ? "active" : ""}
                                onClick={() => setVerdicts((all) => ({ ...all, [c.id]: { ...all[c.id], [id]: x } }))}
                              >
                                {VERDICT_LABEL[x]}
                              </button>
                            ))}
                          </div>
                          {missing.includes(id) && <span className="exp-tag yellow">必须选一种</span>}
                        </div>
                      </div>
                    );
                  })}
            </li>
          );
        })}
      </ul>
    </>
  );
}

function Export({ entries }: { entries: ExpertiseEntry[] }) {
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const text = useMemo(() => exportMarkdown(entries), [entries]);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard may be unavailable; the preview can be selected by hand.
    }
  };
  return (
    <section className="settings-section">
      <div className="settings-head">
        <h2>导出 Markdown</h2>
        <div className="spacer" />
        <button onClick={() => setOpen(!open)} disabled={!text}>
          {open ? "收起预览" : "预览"}
        </button>
        <button onClick={copy} disabled={!text}>
          <Icon name={copied ? "check" : "copy"} size={14} /> {copied ? "已复制" : "复制"}
        </button>
      </div>
      {!text && <div className="empty">没有可导出的条目。</div>}
      {open && text && <pre className="exp-md">{text}</pre>}
    </section>
  );
}

/** Expertise profiles: the card, pending candidates, the profile by domain, export. */
export function ExpertiseView() {
  const [kind, setKind] = useState<OwnerKind>("person");
  const workspace = useSelectedWorkspace();
  const owner = kind === "person" ? PERSON_OWNER_ID : workspace;
  const deps = [kind, owner];
  const entries = useHubData(() => (owner ? api.expertise(kind, owner) : Promise.resolve([])), deps, relevant);
  const card = useHubData(() => (owner ? api.expertiseCard(kind, owner) : Promise.resolve(undefined)), deps, relevant);
  const maint = useHubData(() => (owner ? api.expertiseMaintenance(kind, owner) : Promise.resolve(undefined)), deps, relevant);
  const cands = useHubData(() => api.expertiseCandidates("pending"), [], relevant);
  const list = entries.data ?? [];
  const mine = (cands.data ?? []).filter((c) => c.owner_kind === kind && c.owner_id === owner);
  const reload = () => {
    entries.reload();
    card.reload();
    maint.reload();
    cands.reload();
  };
  return (
    <>
      <div className="topbar">
        <span className="title">专长</span>
      </div>
      <div className="page">
        <div className="page-inner settings exp-page">
          <div className="exp-owner">
            <div className="segmented small">
              <button className={kind === "person" ? "active" : ""} onClick={() => setKind("person")}>
                个人
              </button>
              <button className={kind === "project" ? "active" : ""} onClick={() => setKind("project")}>
                项目
              </button>
            </div>
            <span className="muted">{kind === "person" ? "你的判断，跟着你到每个项目" : owner ? `项目 ${wsName(owner)}` : ""}</span>
          </div>
          {!owner ? (
            <div className="empty">先在侧边栏选一个项目，再看它的专长档案。</div>
          ) : (
            <>
              <ErrorBox error={entries.error ?? card.error ?? maint.error ?? cands.error} />
              {card.data && <Card card={card.data} />}
              <section className="settings-section">
                <div className="settings-head">
                  <h2>候选</h2>
                  {mine.length > 0 && <span className="chip accent">{mine.length}</span>}
                </div>
                <Candidates key={`${kind}:${owner}`} list={mine} entries={list} onDone={reload} />
              </section>
              <section className="settings-section">
                <div className="settings-head">
                  <h2>档案</h2>
                  <span className="muted">按领域分组；每个领域的判断预算 1500 token</span>
                </div>
                {entries.data && <Profile entries={list} hints={maint.data?.stale ?? []} />}
              </section>
              <Export entries={list} />
            </>
          )}
        </div>
      </div>
    </>
  );
}
