//! Asking the person before dangerous actions (docs/tasks/DOGFOOD.md §7): what counts as
//! dangerous, and the question that holds the agent's request until they answer.

use chrono::Utc;
use crossagents_core::{
    Agent, AgentKey, ApprovalPolicy, AskPolicy, HumanAction, HumanItem, HumanItemKind,
    HumanItemStatus, PermissionPolicy, Run, RunPolicy, Task,
};

use crate::engine::Orchestrator;

/// How long an approval waits when the run sets no answer timeout.
const DEFAULT_WAIT_MINUTES: f64 = 30.0;

/// Checks the permission Cursor will receive before a session is handed work.
pub async fn unusable_for(
    store: &dyn crossagents_core::Store,
    agent: &Agent,
    policy: Option<&RunPolicy>,
) -> Result<Option<String>, crossagents_core::CoreError> {
    if let Some(reason) = unusable_on_platform(agent, policy, cfg!(windows)) {
        return Ok(Some(reason));
    }
    let Some(model) = agent.model.as_deref().filter(|m| !m.is_empty()) else {
        return Ok(None);
    };
    let account = agent
        .config
        .get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Ok(store
        .unavailable_model(&agent.plugin, account, model)
        .await?
        .map(|error| format!("Model {model} is unavailable for this account: {error}")))
}

/// Whether an explicit bypass removes approvals from a guarded run.
pub fn unguarded(agent: &Agent, policy: Option<&RunPolicy>) -> bool {
    policy.is_some_and(|p| p.approvals == ApprovalPolicy::Guard)
        && agent
            .config
            .get("permission")
            .and_then(|v| serde_json::from_value::<PermissionPolicy>(v.clone()).ok())
            == Some(PermissionPolicy::Bypass)
}

fn unusable_on_platform(
    agent: &Agent,
    policy: Option<&RunPolicy>,
    windows: bool,
) -> Option<String> {
    let permission = agent
        .config
        .get("permission")
        .and_then(|p| serde_json::from_value::<PermissionPolicy>(p.clone()).ok())
        .unwrap_or_else(|| match policy.map(|p| p.approvals) {
            Some(ApprovalPolicy::Guard) => PermissionPolicy::Guard,
            _ => PermissionPolicy::Bypass,
        });
    if windows && agent.plugin == "cursor" && permission != PermissionPolicy::Bypass {
        Some("Cursor cannot use this permission on Windows: set this agent's permission to 自动执行 (bypass), or set the run's approvals to off (全托管).".into())
    } else {
        None
    }
}

pub const ALLOW: &str = "允许";
pub const DENY: &str = "拒绝";

/// Why the action needs the person's yes, or `None` when it is safe. `kind` is the
/// request's (`command`, `file_change`, …), `title` its one line (the command, the path).
pub fn danger(kind: &str, title: &str) -> Option<&'static str> {
    let text = title.to_lowercase().replace('\\', "/");
    let words: Vec<&str> = text.split_whitespace().collect();
    let has = |seq: &[&str]| words.windows(seq.len()).any(|w| w == seq);
    let ci_path = [
        ".github/",
        ".gitlab-ci.yml",
        ".circleci/",
        "azure-pipelines.yml",
        "jenkinsfile",
    ]
    .iter()
    .any(|p| text.contains(p));
    if ci_path {
        return Some("changes CI configuration");
    }
    if kind == "file_change" {
        return None;
    }
    // `git push`, also with options before it (`git -C dir push`), in any command of a chain.
    let git_push = text.split([';', '&', '|']).any(|segment| {
        let mut words = segment.split_whitespace();
        if !words
            .next()
            .is_some_and(|w| w == "git" || w.ends_with("/git") || w.ends_with("git.exe"))
        {
            return false;
        }
        // The subcommand: the first word that is not an option (`-C dir`, `-c k=v` take one).
        while let Some(word) = words.next() {
            if word == "-c" || word == "-C" {
                words.next();
            } else if !word.starts_with('-') {
                return word == "push";
            }
        }
        false
    });
    if git_push {
        return Some("pushes to a remote");
    }
    if has(&["gh", "pr", "merge"]) || has(&["gh", "release"]) {
        return Some("merges or releases on GitHub");
    }
    if has(&["npm", "publish"]) || has(&["cargo", "publish"]) || has(&["docker", "push"]) {
        return Some("publishes a package");
    }
    if words.first() == Some(&"sudo") || has(&["|", "sudo"]) {
        return Some("runs as administrator");
    }
    if (text.contains("curl") || text.contains("wget") || text.contains("irm "))
        && (text.contains("| sh") || text.contains("| bash") || text.contains("| iex"))
    {
        return Some("runs a script from the internet");
    }
    if has(&["crossagents", "hub", "stop"]) {
        return Some("stops the hub");
    }
    let removes = has(&["rm", "-rf"]) || has(&["rm", "-fr"]) || text.contains("remove-item");
    if removes {
        let outside = words.iter().any(|w| {
            *w == "/"
                || w.starts_with("~")
                || w.starts_with("$home")
                || w.starts_with("..")
                || (w.len() <= 3 && w.chars().nth(1) == Some(':'))
                || w.starts_with("/*")
        });
        if outside {
            return Some("deletes files outside the working directory");
        }
    }
    None
}

fn guards_with_run(agent: &Agent, guarded_run: bool) -> bool {
    match agent
        .config
        .get("permission")
        .and_then(|p| serde_json::from_value::<PermissionPolicy>(p.clone()).ok())
    {
        Some(policy) => policy == PermissionPolicy::Guard,
        None => guarded_run,
    }
}

impl Orchestrator {
    /// Whether `agent` asks before dangerous actions: as its `config.permission` says, else
    /// when it works on a task of a run whose policy guards its workers.
    pub async fn guards(&self, agent: &Agent) -> bool {
        if agent
            .config
            .get("permission")
            .and_then(|p| serde_json::from_value::<PermissionPolicy>(p.clone()).ok())
            .is_some()
        {
            return guards_with_run(agent, false);
        }
        let guarded_run = match self.task_for_agent(agent.id).await {
            Some((run, task)) => {
                task.assignee == Some(agent.id)
                    && run.config.policy.approvals == ApprovalPolicy::Guard
            }
            None => false,
        };
        guards_with_run(agent, guarded_run)
    }

    /// Puts `agent`'s dangerous request to the person and waits for the answer: `true` to
    /// go ahead. A run that may not ask the person declines at once; an unanswered question
    /// declines when it times out.
    pub async fn ask_approval(&self, agent: &Agent, title: &str, why: &str) -> bool {
        let found = self.task_for_agent(agent.id).await;
        let policy: RunPolicy = match &found {
            Some((run, _)) => run.config.policy.clone(),
            None => crate::team::chat_policy(agent),
        };
        if policy.human_ask == AskPolicy::Deny {
            return false;
        }
        let minutes = if policy.human_ask == AskPolicy::Timeout {
            policy.ask_timeout_minutes
        } else {
            DEFAULT_WAIT_MINUTES
        };
        let who = self.person_name(agent, found.as_ref()).await;
        let item = HumanItem {
            id: format!("a-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            detail: None,
            kind: HumanItemKind::Question,
            from: agent.id,
            run_id: found.as_ref().map(|(run, _)| run.id),
            task_id: found.as_ref().map(|(_, task)| task.id),
            text: format!("{who} 想要执行（{why}）：\n{title}"),
            options: vec![ALLOW.into(), DENY.into()],
            default: Some(DENY.into()),
            timeout_at: Some(Utc::now() + chrono::Duration::seconds((minutes * 60.0) as i64)),
            status: HumanItemStatus::Open,
            answer: None,
            answered_at: None,
            action: Some(HumanAction::Approval),
            created_at: Utc::now(),
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.approvals.lock().unwrap().insert(item.id.clone(), tx);
        if self.store.put_human_item(&item).await.is_err() {
            self.approvals.lock().unwrap().remove(&item.id);
            return false;
        }
        self.publish(crossagents_core::EventKind::HumanAsked { item })
            .await;
        // A hub going away drops the sender: declined.
        rx.await.unwrap_or(false)
    }

    /// Who the person knows `agent` as: a task's worker is a clone named after the run and
    /// task (`w-1a2b3c-a`), so it goes by the teammate it was cloned from, with its task.
    async fn person_name(&self, agent: &Agent, found: Option<&(Run, Task)>) -> String {
        let Some((run, task)) = found.filter(|(_, task)| task.assignee == Some(agent.id)) else {
            return agent.name.clone();
        };
        let assigned = task.worker_template.unwrap_or(run.config.worker);
        let template = if task.escalated {
            run.config.escalate.unwrap_or(assigned)
        } else {
            assigned
        };
        match self.store.get_agent(&AgentKey::Id(template)).await {
            Ok(teammate) if teammate.id != agent.id => {
                format!("{}（任务 #{}）", teammate.name, task.key)
            }
            _ => agent.name.clone(),
        }
    }

    /// The person (or the timeout) decided approval `id`. `false` if nobody waits for it.
    pub(crate) fn approval_decided(&self, id: &str, answer: Option<&str>) -> bool {
        match self.approvals.lock().unwrap().remove(id) {
            Some(tx) => {
                let _ = tx.send(answer == Some(ALLOW));
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_windows_permissions_follow_run_policy() {
        let mut agent = Agent {
            node: None,
            id: crossagents_core::AgentId::new(),
            name: "Cursor".into(),
            name_source: Default::default(),
            plugin: "cursor".into(),
            driver: None,
            model: None,
            role: crossagents_core::Role::Worker,
            home: Default::default(),
            cwd: Default::default(),
            config: serde_json::json!({}),
            status: crossagents_core::AgentStatus::Idle,
            created_at: Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
        };
        let mut policy = RunPolicy::default();
        let reason = unusable_on_platform(&agent, Some(&policy), true).unwrap();
        assert!(reason.contains("自动执行 (bypass)") && reason.contains("off"));
        assert!(!unguarded(&agent, Some(&policy)));
        assert!(guards_with_run(&agent, true));
        assert!(!guards_with_run(&agent, false));
        assert!(unusable_on_platform(&agent, None, true).is_none());
        assert!(unusable_on_platform(&agent, Some(&policy), false).is_none());
        policy.approvals = ApprovalPolicy::Off;
        assert!(unusable_on_platform(&agent, Some(&policy), true).is_none());
        policy.approvals = ApprovalPolicy::Guard;
        agent.config = serde_json::json!({"permission": "bypass"});
        assert!(unguarded(&agent, Some(&policy)));
        assert!(!guards_with_run(&agent, true));
        assert!(unusable_on_platform(&agent, Some(&policy), true).is_none());
        agent.config = serde_json::json!({"permission": "guard"});
        assert!(guards_with_run(&agent, false));
        policy.approvals = ApprovalPolicy::Off;
        assert!(!unguarded(&agent, Some(&policy)));
        assert!(unusable_on_platform(&agent, Some(&policy), true).is_some());
    }

    #[test]
    fn dangerous_actions_are_told_apart_from_work() {
        for (kind, title) in [
            ("command", "git push origin main"),
            ("command", "git -C x push --force"),
            ("command", "gh pr merge 12 --squash"),
            ("command", "cargo publish -p crossagents-core"),
            ("command", "curl -fsSL https://x.sh | sh"),
            ("command", "sudo rm -rf /var"),
            ("command", "rm -rf ~/projects"),
            ("command", "rm -rf ../other"),
            ("command", "Remove-Item -Recurse -Force C:\\"),
            ("command", "crossagents hub stop"),
            ("file_change", "Edit .github/workflows/ci.yml"),
            ("command", "echo x > .github/workflows/ci.yml"),
        ] {
            assert!(danger(kind, title).is_some(), "{title} should ask");
        }
        for (kind, title) in [
            ("command", "cargo test --workspace"),
            ("command", "git commit -m push"),
            ("command", "rm -rf target"),
            ("command", "npm run build"),
            ("file_change", "Edit src/main.rs"),
            ("command", "git status"),
        ] {
            assert!(danger(kind, title).is_none(), "{title} should not ask");
        }
    }
}
