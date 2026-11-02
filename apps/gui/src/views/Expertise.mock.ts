// Mock data for the expertise page (preview:mock, docs EXPERTISE.md): a person's profile and a
// project's, with a domain over its judgement budget, stale/review entries, one with a high
// rework rate, pending candidates (one in conflict, one the lead suggested), and a script that
// walks one task through: pushed on dispatch → outcome filled on delivery → a rework turns into a
// candidate → the person accepts it → the card moves.

import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { ago, REPO, team, uid } from "../dev/fixtures";
import {
  acceptCandidate,
  applyOutcome,
  PERSON_OWNER_ID,
  type Candidate,
  type EntryKind,
  type EntryStatus,
  type Example,
  type ExpertiseEntry,
  type OwnerKind,
  type UsageOutcome,
  type UsageRecord,
} from "../lib/expertise";
import { wsKey } from "../workspace";
import { quota } from "./Usage.mock";

const DAY = 60 * 24;
export const PROJECT_OWNER = wsKey(REPO);
const RUN = "01a1-run-login";

type Init = {
  domain: string;
  kind: EntryKind;
  title: string;
  text?: string;
  example?: Example;
  applies_when?: string;
  status?: EntryStatus;
  stats?: [number, number, number, number];
  daysOld?: number;
  confirmedDaysAgo?: number | null;
};

function entry(id: string, owner_kind: OwnerKind, owner_id: string, i: Init): ExpertiseEntry {
  const [used, merged_first_try, reworked, dropped] = i.stats ?? [0, 0, 0, 0];
  const created = ago((i.daysOld ?? 20) * DAY);
  return {
    id,
    owner_kind,
    owner_id,
    domain: i.domain,
    kind: i.kind,
    title: i.title,
    text: i.text ?? "",
    example: i.example ?? null,
    applies_when: i.applies_when ?? "",
    evidence: merged_first_try + reworked,
    last_confirmed_at: i.confirmedDaysAgo === null ? null : ago((i.confirmedDaysAgo ?? 3) * DAY),
    status: i.status ?? "active",
    superseded_by: null,
    stats: { used, merged_first_try, reworked, dropped },
    sources: [{ kind: "run", run_id: RUN }],
    created_at: created,
    updated_at: created,
  };
}

const person = (id: string, i: Init) => entry(id, "person", PERSON_OWNER_ID, i);
const project = (id: string, i: Init) => entry(id, "project", PROJECT_OWNER, i);

// Long judgements so the "前端" domain goes over its 1500-token budget.
const long = (s: string, n: number) => Array.from({ length: n }, () => s).join("");

function entries(): ExpertiseEntry[] {
  return [
    person("p-err", {
      domain: "后端",
      kind: "judgment",
      title: "错误要带上下文再往上抛",
      text: "包一层 anyhow::Context，写清在做什么、对哪个对象；不要只把底层错误原样返回。",
      applies_when: "crates/**/*.rs",
      stats: [6, 5, 1, 0],
    }),
    person("p-sql", {
      domain: "后端",
      kind: "judgment",
      title: "迁移只加不改",
      text: "已发布的迁移文件不再改动，修正写新的迁移。",
      applies_when: "migrations/**",
      stats: [4, 4, 0, 0],
    }),
    person("p-lock", {
      domain: "后端",
      kind: "judgment",
      title: "持锁时不做 IO",
      text: "拿到 Mutex 之后只改内存，写盘和网络请求放到锁外。",
      stats: [5, 2, 3, 0],
    }),
    person("p-retry-ex", {
      domain: "后端",
      kind: "example",
      title: "重试放在调用方",
      example: {
        situation: "上游接口偶尔 502",
        rejected: "在客户端库里无限重试",
        rejected_why: "调用方无法控制超时，测试也会卡住",
        chosen: "库里只报错，调用方按场景决定重试次数与退避",
      },
      stats: [2, 2, 0, 0],
    }),
    person("p-limit-unsafe", {
      domain: "后端",
      kind: "limit",
      title: "不写 unsafe",
      text: "需要时先停下来问，不自己决定。",
    }),
    person("p-css", {
      domain: "前端",
      kind: "judgment",
      title: "颜色只用 CSS 变量",
      text: long("新样式里的颜色、边框和背景一律取自主题变量，比如表面色、边框色、强调色和警告色，这样浅色与深色主题能同时生效，也便于以后统一调整。", 9),
      applies_when: "apps/gui/src/**/*.css",
      stats: [7, 6, 1, 0],
    }),
    person("p-state", {
      domain: "前端",
      kind: "judgment",
      title: "页面数据走 useHubData",
      text: long("页面里取 hub 数据统一用 useHubData，给出相关事件的判断函数，让事件到来时自动重新加载，而不是自己写轮询或者在组件里散落请求和定时器。", 9),
      stats: [5, 4, 1, 0],
    }),
    person("p-a11y", {
      domain: "前端",
      kind: "judgment",
      title: "可点的东西用 button",
      text: long("能点击的元素用按钮或链接，并写上无障碍标签，键盘能聚焦能触发，不要给普通的块元素绑定点击事件来冒充按钮。", 10),
      stats: [3, 3, 0, 0],
      status: "stale",
      confirmedDaysAgo: 75,
      daysOld: 90,
    }),
    person("p-i18n", {
      domain: "前端",
      kind: "limit",
      title: "导航文案进 i18n",
      text: "侧边栏和标题的文字写进 i18n 表，页面正文可以直接写中文。",
      status: "review",
      stats: [2, 1, 1, 0],
    }),
    project("s-pay", {
      domain: "支付",
      kind: "judgment",
      title: "金额用整数分",
      text: "所有金额以分为单位的整数存储和计算，展示时再格式化。",
      applies_when: "src/payments/**",
      stats: [8, 7, 1, 0],
    }),
    project("s-idem", {
      domain: "支付",
      kind: "judgment",
      title: "回调要幂等",
      text: "支付回调按订单号去重，重复回调直接返回成功。",
      stats: [4, 1, 2, 1],
    }),
    project("s-pay-ex", {
      domain: "支付",
      kind: "example",
      title: "退款不删订单",
      example: {
        situation: "用户申请全额退款",
        rejected: "删除订单记录",
        rejected_why: "对账需要原始订单，审计也要求保留",
        chosen: "订单标记为已退款，另写一条退款流水",
      },
    }),
    project("s-test", {
      domain: "测试",
      kind: "judgment",
      title: "集成测试用真实数据库",
      text: "用 docker 起的 Postgres，不用内存替身。",
      stats: [3, 3, 0, 0],
      confirmedDaysAgo: 70,
      daysOld: 120,
    }),
    project("s-test-limit", {
      domain: "测试",
      kind: "limit",
      title: "不改快照来让测试通过",
      text: "快照不一致先找原因，改快照要在总结里说明。",
      status: "stale",
      confirmedDaysAgo: 80,
      daysOld: 120,
    }),
  ];
}

function usages(list: ExpertiseEntry[]): UsageRecord[] {
  // Spread each entry's history: the older part (more than 30 days ago) with more rework, so
  // the first-try rate has risen since.
  const out: UsageRecord[] = [];
  for (const e of list) {
    const { merged_first_try, reworked, dropped } = e.stats;
    const outcomes: UsageOutcome[] = [
      ...Array<UsageOutcome>(reworked).fill("reworked"),
      ...Array<UsageOutcome>(dropped).fill("dropped"),
      ...Array<UsageOutcome>(merged_first_try).fill("merged_first_try"),
    ];
    outcomes.forEach((outcome, i) => {
      const daysAgo = i < Math.ceil(outcomes.length / 2) ? 35 + i * 4 : 2 + i * 3;
      const recentRework = e.id === "p-i18n" && outcome === "reworked";
      out.push({
        id: uid(),
        entry_id: e.id,
        run_id: RUN,
        task_id: `t-${e.id}-${i}`,
        agent: "rex",
        source: "pushed",
        outcome,
        at: ago((recentRework ? 4 : daysAgo) * DAY),
      });
    });
  }
  return out;
}

function candidates(list: ExpertiseEntry[]): Candidate[] {
  const base = list.find((e) => e.id === "p-lock")!;
  const proposed = (i: Init) => ({ ...person("proposed", i), stats: { used: 0, merged_first_try: 0, reworked: 0, dropped: 0 }, evidence: 1 });
  return [
    {
      id: "c-lock",
      owner_kind: "person",
      owner_id: PERSON_OWNER_ID,
      domain: "后端",
      proposed: proposed({
        domain: "后端",
        kind: "judgment",
        title: "持锁时不做 IO，也不 await",
        text: "拿到锁之后只改内存；写盘、网络请求和任何 .await 都放到锁外，用 tokio::sync 的锁时也一样。",
      }),
      conflicts_with: [base.id],
      origin: "rework",
      status: "pending",
      suggested_by_lead: true,
      note: "Ada：#auth 返工两次都是锁里 await 了，旧条目没说到 async，建议替换。",
      sources: [{ kind: "task", run_id: RUN, task_id: "t-auth" }],
      created_at: ago(90),
      decided_at: null,
    },
    {
      id: "c-log",
      owner_kind: "person",
      owner_id: PERSON_OWNER_ID,
      domain: "后端",
      proposed: proposed({ domain: "后端", kind: "judgment", title: "日志不打印令牌", text: "token、cookie、密码只记前 4 位。" }),
      conflicts_with: [],
      origin: "review",
      status: "pending",
      suggested_by_lead: false,
      note: "",
      sources: [{ kind: "task", run_id: RUN, task_id: "t-session" }],
      created_at: ago(60),
      decided_at: null,
    },
    {
      id: "c-empty",
      owner_kind: "person",
      owner_id: PERSON_OWNER_ID,
      domain: "前端",
      proposed: proposed({ domain: "前端", kind: "limit", title: "空状态要写下一步", text: "列表为空时说明怎么才会有内容。" }),
      conflicts_with: [],
      origin: "merged_first_try",
      status: "pending",
      suggested_by_lead: true,
      note: "Ada：这次一次合并就是因为空状态写清了，值得记下。",
      sources: [{ kind: "task", run_id: RUN, task_id: "t-list" }],
      created_at: ago(30),
      decided_at: null,
    },
    {
      id: "c-refund",
      owner_kind: "project",
      owner_id: PROJECT_OWNER,
      domain: "支付",
      proposed: { ...project("proposed", { domain: "支付", kind: "judgment", title: "回调先验签再去重", text: "验签失败直接 401，不进入去重逻辑。" }), evidence: 1 },
      conflicts_with: ["s-idem"],
      origin: "rework",
      status: "pending",
      suggested_by_lead: false,
      note: "",
      sources: [{ kind: "task", run_id: RUN, task_id: "t-callback" }],
      created_at: ago(20),
      decided_at: null,
    },
  ];
}

/** Find an entry and replace it with `f(entry)`. */
function update(s: MockState, id: string, f: (e: ExpertiseEntry) => ExpertiseEntry) {
  const x = s.expertise!;
  x.entries = x.entries.map((e) => (e.id === id ? f(e) : e));
}

const ping = (text: string): Step["event"] => ({ type: "task.progress", data: { run_id: RUN, id: "t-demo", key: "demo", text } });

/** The expertise page with its scripted walk through one task. */
export function expertiseScene(): { state: MockState; script: Step[] } {
  const { ada, rex, mia, sam } = team();
  const list = entries();
  const state: MockState = {
    ...emptyState(),
    agents: [ada, rex, mia, sam],
    quota: quota(),
    expertise: { entries: list, candidates: candidates(list), usages: usages(list) },
  };
  localStorage.setItem("crossagents.workspace", PROJECT_OWNER);
  const pushed = ["p-err", "p-sql", "p-lock"];
  const ids = new Map(pushed.map((id) => [id, uid()]));
  const script: Step[] = [
    {
      // Dispatch: the hub pushes three judgements with the brief; no outcome yet.
      at: 1500,
      act: (s) => {
        for (const id of pushed) {
          s.expertise!.usages.push({ id: ids.get(id)!, entry_id: id, run_id: RUN, task_id: "t-demo", agent: "rex", source: "pushed", outcome: null, at: new Date().toISOString() });
        }
      },
      event: ping("派活：推送 3 条判断给 rex"),
    },
    {
      // Delivery: two of them merged first try.
      at: 4000,
      act: (s) => {
        const at = new Date().toISOString();
        for (const id of ["p-err", "p-sql"]) {
          const u = s.expertise!.usages.find((x) => x.id === ids.get(id))!;
          u.outcome = "merged_first_try";
          u.at = at;
          update(s, id, (e) => applyOutcome(e, "merged_first_try", at));
        }
      },
      event: ping("交活：2 条一次合并"),
    },
    {
      // Rework: the lock judgement did not hold; the rework turns into a candidate.
      at: 6500,
      act: (s) => {
        const at = new Date().toISOString();
        const u = s.expertise!.usages.find((x) => x.id === ids.get("p-lock"))!;
        u.outcome = "reworked";
        u.at = at;
        update(s, "p-lock", (e) => applyOutcome(e, "reworked", at));
        const c = s.expertise!.candidates.find((x) => x.id === "c-lock")!;
        c.note = "Ada：#demo 又因为锁里 await 返工了（第 3 次），建议替换旧条目。";
        c.created_at = at;
      },
      event: ping("返工：生成候选「持锁时不做 IO，也不 await」"),
    },
    {
      // The person accepts it, replacing the old entry.
      at: 8500,
      act: (s) => {
        const x = s.expertise!;
        const c = x.candidates.find((k) => k.id === "c-lock")!;
        const at = new Date().toISOString();
        x.entries = acceptCandidate(x.entries, c, [{ entry_id: "p-lock", verdict: "replace" }], uid(), at);
        c.status = "accepted";
        c.decided_at = at;
      },
      event: ping("接受候选：替换旧条目"),
    },
  ];
  return { state, script };
}
