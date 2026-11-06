// Projects on the 排障记录 page (docs/tasks/LESSONS.md §4.2, L3): the records belong to a
// project, not to a directory, so a person confirms here what the hub could not — a remote
// that appeared at a checkout, a directory that belongs to another project already, and two
// projects that turn out to be one.

import { useState } from "react";
import { api, errorText } from "../../api";
import { ErrorBox, Icon } from "../../components";
import type { HubEvent } from "../../hub";
import { useHubData } from "../../hub";
import type { ProjectInfo } from "../../lib/expertise";
import { shortId } from "../../lessons/lessons";

const relevant = (e: HubEvent) => e.type === "run.created" || e.type === "lesson.created" || e.type === "lesson.updated";

/** A project's checkouts on this node (a project with none has been merged away). */
const live = (p: ProjectInfo) => p.paths.length > 0;

/** One project: what identifies it, and what the person can settle about it. */
function ProjectRow({
  p,
  open,
  onToggle,
  onBind,
  busy,
}: {
  p: ProjectInfo;
  open: boolean;
  onToggle: () => void;
  onBind: (req: { remote?: string; path?: string; split?: boolean }) => void;
  busy: boolean;
}) {
  const [remote, setRemote] = useState("");
  const [path, setPath] = useState("");
  return (
    <li className="project-row">
      <div className="project-head">
        <code className="muted" title={p.id}>
          {shortId(p.id)}
        </code>
        <b>{p.name}</b>
        {p.remotes.map((r) => (
          <code key={r} title="git remote">
            {r}
          </code>
        ))}
        {p.unconfirmed_remote && <span className="exp-tag yellow">身份待确认</span>}
        <span className="spacer" />
        <button onClick={onToggle} aria-expanded={open}>
          <Icon name={open ? "chevron" : "chevronRight"} size={14} /> 绑定…
        </button>
      </div>
      <div className="project-paths muted">{p.paths.map((at) => at.path).join(" · ")}</div>
      {p.unconfirmed_remote && (
        <div className="project-ask">
          <span>
            这个目录现在指向 <code>{p.unconfirmed_remote}</code>，不是这个项目认识的 remote。是改名或迁移吗？
          </span>
          <button disabled={busy} onClick={() => onBind({ remote: p.unconfirmed_remote ?? undefined })}>
            是，记作它的别名
          </button>
          {p.paths.map((at) => (
            <button key={at.path} disabled={busy} title={`把 ${at.path} 分成另一个项目`} onClick={() => onBind({ path: at.path, split: true })}>
              不是，把 {at.path.split(/[\\/]/).pop() || at.path} 分成另一个项目
            </button>
          ))}
        </div>
      )}
      {open && (
        <form
          className="project-bind"
          onSubmit={(e) => {
            e.preventDefault();
            const req: { remote?: string; path?: string } = {};
            if (remote.trim()) req.remote = remote.trim();
            if (path.trim()) req.path = path.trim();
            if (req.remote || req.path) onBind(req);
          }}
        >
          <input
            value={remote}
            onChange={(e) => setRemote(e.target.value)}
            placeholder="remote：git@host:owner/repo 或 https://…"
            aria-label="remote"
          />
          <input
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="本机目录（会把该目录改绑到这个项目）"
            aria-label="本机目录"
          />
          <button type="submit" disabled={busy || (!remote.trim() && !path.trim())}>
            绑定
          </button>
        </form>
      )}
    </li>
  );
}

/** The projects, what identifies them, and the two decisions only a person can make. */
export function ProjectsPanel() {
  const [tick, setTick] = useState(0);
  const projects = useHubData(() => api.projects(), [tick], relevant);
  const [open, setOpen] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [from, setFrom] = useState("");
  const [into, setInto] = useState("");

  const all = projects.data ?? [];
  const list = all.filter(live);
  const act = async (work: () => Promise<unknown>, done: string) => {
    setBusy(true);
    setError(undefined);
    setNotice(undefined);
    try {
      await work();
      setNotice(done);
      setTick((n) => n + 1);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };
  const mergeable = list.length >= 2;

  return (
    <section className="settings-section project-panel">
      <div className="settings-head">
        <h2>项目</h2>
        {projects.data && <span className="muted">{list.length} 个</span>}
      </div>
      <p className="muted project-note">
        排障记录跟着项目走，不跟目录走：同一个仓库换个目录、换台机器，看到的是同一批记录。换了 remote 或者发现两个项目其实是一个，在这里由人确认。
      </p>
      <ErrorBox error={projects.error ?? error} />
      {notice && <div className="project-notice">{notice}</div>}
      {projects.data && list.length === 0 && <div className="empty">还没有认识的项目。</div>}
      <ul className="project-list">
        {list.map((p) => (
          <ProjectRow
            key={p.id}
            p={p}
            open={open === p.id}
            busy={busy}
            onToggle={() => setOpen(open === p.id ? null : p.id)}
            onBind={(req) =>
              void act(
                () => api.bindProject(p.id, req),
                `已绑定到「${p.name}」。${req.path ? "这个目录之后的 run 都算这个项目。" : ""}`,
              )
            }
          />
        ))}
      </ul>
      {mergeable && (
        <div className="project-merge">
          <span>把</span>
          <select value={from} onChange={(e) => setFrom(e.target.value)} aria-label="要并入的项目">
            <option value="">选一个…</option>
            {list.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}（{shortId(p.id)}）
              </option>
            ))}
          </select>
          <span>并入</span>
          <select value={into} onChange={(e) => setInto(e.target.value)} aria-label="保留的项目">
            <option value="">选一个…</option>
            {list
              .filter((p) => p.id !== from)
              .map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}（{shortId(p.id)}）
                </option>
              ))}
          </select>
          <button
            disabled={busy || !from || !into || from === into}
            onClick={() =>
              act(async () => {
                const kept = await api.mergeProjects({ from, into });
                setFrom("");
                setInto("");
                return kept;
              }, "两个项目已合并：前者的记录、目录和 remote 都归到了后者。")
            }
          >
            合并
          </button>
        </div>
      )}
    </section>
  );
}
