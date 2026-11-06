// 接力链 at the top of a run page (docs/research/magpie-lancer.md §2.4): one line per task —
// `#a  OAuth 回调  Rex ✓ → Ada → 你  ○ 待检查` — so who holds a task and where it waits on the
// person read at a glance. Pure data in lib/relay.ts.

import type { Agent } from "@bindings/Agent";
import type { Run } from "@bindings/Run";
import type { Task } from "@bindings/Task";
import { Avatar } from "./components";
import { relayOf, relayText } from "./lib/relay";
import { taskState } from "./lib/taskState";
import { StateWord } from "./parts";
import "./relay.css";

export function RelayBanner({ run, tasks, agents }: { run: Run; tasks: Task[]; agents?: Agent[] }) {
  if (!tasks.length) return null;
  return (
    <div className="relay-banner" role="list" aria-label="接力链">
      {tasks.map((task) => {
        const relay = relayOf(task, run, agents);
        return (
          <a
            key={task.id}
            role="listitem"
            className={`relay-line ${relay.needsYou ? "waits-you" : ""}`}
            href={`#/runs/${run.id}/tasks/${task.id}`}
            title={`#${task.key} ${task.title}\n${relayText(relay)}`}
          >
            <code className="relay-key">#{task.key}</code>
            <span className="relay-title">{task.title}</span>
            <span className="relay-chain" aria-label={relayText(relay)}>
              {relay.legs.map((leg, i) => (
                <span key={`${leg.name}-${i}`} className="relay-hop">
                  {i > 0 && (
                    <span className="relay-arrow" aria-hidden="true">
                      →
                    </span>
                  )}
                  <span className={`relay-leg ${leg.state}`}>
                    {leg.agent && <Avatar agent={leg.agent} size={15} />}
                    <span className="relay-name">{leg.name}</span>
                    <span className="relay-glyph" aria-hidden="true">
                      {leg.glyph}
                    </span>
                  </span>
                </span>
              ))}
            </span>
            <span className="spacer" />
            <StateWord state={taskState(task, tasks)} />
          </a>
        );
      })}
    </div>
  );
}
