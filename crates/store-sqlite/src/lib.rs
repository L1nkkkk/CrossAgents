//! SQLite implementation of [`crossagents_core::Store`].

use std::path::{Path, PathBuf};
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Account, Agent, AgentId, AgentKey, AgentProfile, AgentStatus, CoreError, EVENT_SCHEMA_VERSION,
    Event, EventKind, MessageId, Reminder, Run, RunId, Store, Task, TaskId, Watch,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Opens (creating if needed) the database file and applies pending migrations.
    pub async fn open(path: &Path) -> Result<Self, CoreError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(storage)?;
        Self::migrate(pool).await
    }

    /// A private in-memory database, for tests.
    pub async fn open_in_memory() -> Result<Self, CoreError> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(storage)?
            .foreign_keys(true);
        // Every connection to `:memory:` is a separate database, so use exactly one.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(storage)?;
        Self::migrate(pool).await
    }

    async fn migrate(pool: SqlitePool) -> Result<Self, CoreError> {
        MIGRATOR.run(&pool).await.map_err(storage)?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

fn storage(err: impl std::fmt::Display) -> CoreError {
    CoreError::Storage(err.to_string())
}

/// Serializes a unit-like serde enum (e.g. `Role`) to its string form.
fn enum_str<T: Serialize>(value: &T) -> Result<String, CoreError> {
    match serde_json::to_value(value).map_err(storage)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(storage(format!("expected string enum, got {other}"))),
    }
}

fn enum_from<T: DeserializeOwned>(s: &str) -> Result<T, CoreError> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(storage)
}

fn time_from(s: &str) -> Result<DateTime<Utc>, CoreError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(storage)
}

fn agent_from_row(row: &SqliteRow) -> Result<Agent, CoreError> {
    let get = |col: &str| row.try_get::<String, _>(col).map_err(storage);
    let get_opt = |col: &str| row.try_get::<Option<String>, _>(col).map_err(storage);
    Ok(Agent {
        id: get("id")?.parse().map_err(storage)?,
        name: get("name")?,
        plugin: get("plugin")?,
        driver: get_opt("driver")?,
        model: get_opt("model")?,
        role: enum_from(&get("role")?)?,
        cwd: PathBuf::from(get("cwd")?),
        config: serde_json::from_str(&get("config")?).map_err(storage)?,
        status: enum_from(&get("status")?)?,
        created_at: time_from(&get("created_at")?)?,
        profile: serde_json::from_str(&get("profile")?).map_err(storage)?,
    })
}

fn event_from_row(row: &SqliteRow) -> Result<Event, CoreError> {
    let get = |col: &str| row.try_get::<String, _>(col).map_err(storage);
    let data: serde_json::Value = serde_json::from_str(&get("data")?).map_err(storage)?;
    let mut tagged = serde_json::json!({ "type": get("type")? });
    if !data.is_null() {
        tagged["data"] = data;
    }
    Ok(Event {
        seq: row.try_get::<i64, _>("seq").map_err(storage)? as u64,
        id: Uuid::parse_str(&get("id")?).map_err(storage)?,
        ts: time_from(&get("ts")?)?,
        schema: row.try_get::<i64, _>("schema").map_err(storage)? as u32,
        kind: serde_json::from_value(tagged).map_err(storage)?,
    })
}

fn not_found(key: &AgentKey) -> CoreError {
    let id = match key {
        AgentKey::Id(id) => id.to_string(),
        AgentKey::Name(name) => name.clone(),
    };
    CoreError::NotFound { kind: "agent", id }
}

#[async_trait]
impl Store for SqliteStore {
    async fn insert_agent(&self, agent: &Agent) -> Result<(), CoreError> {
        let result = sqlx::query(
            "INSERT INTO agents (id, name, plugin, driver, model, role, cwd, config, status, created_at, profile)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(agent.id.to_string())
        .bind(&agent.name)
        .bind(&agent.plugin)
        .bind(&agent.driver)
        .bind(&agent.model)
        .bind(enum_str(&agent.role)?)
        .bind(agent.cwd.to_string_lossy().into_owned())
        .bind(serde_json::to_string(&agent.config).map_err(storage)?)
        .bind(enum_str(&agent.status)?)
        .bind(agent.created_at.to_rfc3339())
        .bind(serde_json::to_string(&agent.profile).map_err(storage)?)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(CoreError::Conflict(
                format!("agent name {:?} is already taken", agent.name),
            )),
            Err(e) => Err(storage(e)),
        }
    }

    async fn get_agent(&self, key: &AgentKey) -> Result<Agent, CoreError> {
        let query = match key {
            AgentKey::Id(id) => {
                sqlx::query("SELECT * FROM agents WHERE id = ?").bind(id.to_string())
            }
            AgentKey::Name(name) => {
                sqlx::query("SELECT * FROM agents WHERE name = ?").bind(name.clone())
            }
        };
        let row = query.fetch_optional(&self.pool).await.map_err(storage)?;
        row.as_ref()
            .map(agent_from_row)
            .unwrap_or_else(|| Err(not_found(key)))
    }

    async fn list_agents(&self) -> Result<Vec<Agent>, CoreError> {
        sqlx::query("SELECT * FROM agents ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?
            .iter()
            .map(agent_from_row)
            .collect()
    }

    async fn delete_agent(&self, id: AgentId) -> Result<(), CoreError> {
        let result = sqlx::query("DELETE FROM agents WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(not_found(&AgentKey::Id(id)));
        }
        Ok(())
    }

    async fn update_agent_profile(
        &self,
        id: AgentId,
        profile: &AgentProfile,
    ) -> Result<(), CoreError> {
        let result = sqlx::query("UPDATE agents SET profile = ? WHERE id = ?")
            .bind(serde_json::to_string(profile).map_err(storage)?)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(not_found(&AgentKey::Id(id)));
        }
        Ok(())
    }

    async fn update_agent_model(&self, id: AgentId, model: Option<&str>) -> Result<(), CoreError> {
        let result = sqlx::query("UPDATE agents SET model = ? WHERE id = ?")
            .bind(model)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(not_found(&AgentKey::Id(id)));
        }
        Ok(())
    }

    async fn update_agent_status(&self, id: AgentId, status: AgentStatus) -> Result<(), CoreError> {
        let result = sqlx::query("UPDATE agents SET status = ? WHERE id = ?")
            .bind(enum_str(&status)?)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(not_found(&AgentKey::Id(id)));
        }
        Ok(())
    }

    async fn append_event(&self, kind: EventKind) -> Result<Event, CoreError> {
        let id = Uuid::now_v7();
        let ts = Utc::now();
        let mut tagged = serde_json::to_value(&kind).map_err(storage)?;
        let data = tagged
            .get_mut("data")
            .map(serde_json::Value::take)
            .unwrap_or(serde_json::Value::Null);
        let seq: i64 = sqlx::query_scalar(
            "INSERT INTO events (id, ts, schema, type, data) VALUES (?, ?, ?, ?, ?) RETURNING seq",
        )
        .bind(id.to_string())
        .bind(ts.to_rfc3339())
        .bind(i64::from(EVENT_SCHEMA_VERSION))
        .bind(kind.type_name())
        .bind(data.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        Ok(Event {
            seq: seq as u64,
            id,
            ts,
            schema: EVENT_SCHEMA_VERSION,
            kind,
        })
    }

    async fn latest_event_seq(&self) -> Result<u64, CoreError> {
        let seq: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM events")
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        Ok(seq.unwrap_or(0) as u64)
    }

    async fn events_since(&self, after: u64, limit: u32) -> Result<Vec<Event>, CoreError> {
        sqlx::query("SELECT * FROM events WHERE seq > ? ORDER BY seq LIMIT ?")
            .bind(after as i64)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?
            .iter()
            .map(event_from_row)
            .collect()
    }

    async fn insert_reminder(&self, reminder: &Reminder) -> Result<(), CoreError> {
        sqlx::query("INSERT INTO reminders (id, owner, due_at, data) VALUES (?, ?, ?, ?)")
            .bind(&reminder.id)
            .bind(reminder.owner.to_string())
            .bind(reminder.due_at.to_rfc3339())
            .bind(serde_json::to_string(reminder).map_err(storage)?)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn list_reminders(&self, owner: Option<AgentId>) -> Result<Vec<Reminder>, CoreError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT data FROM reminders WHERE ?1 IS NULL OR owner = ?1 ORDER BY due_at",
        )
        .bind(owner.map(|o| o.to_string()))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn delete_reminder(&self, id: &str) -> Result<(), CoreError> {
        let result = sqlx::query("DELETE FROM reminders WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "reminder",
                id: id.to_owned(),
            });
        }
        Ok(())
    }

    async fn insert_watch(&self, watch: &Watch) -> Result<(), CoreError> {
        sqlx::query("INSERT INTO watches (id, owner, data) VALUES (?, ?, ?)")
            .bind(&watch.id)
            .bind(watch.owner.to_string())
            .bind(serde_json::to_string(watch).map_err(storage)?)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn list_watches(&self, owner: Option<AgentId>) -> Result<Vec<Watch>, CoreError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT data FROM watches WHERE ?1 IS NULL OR owner = ?1 ORDER BY id",
        )
        .bind(owner.map(|o| o.to_string()))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn delete_watch(&self, id: &str) -> Result<(), CoreError> {
        let result = sqlx::query("DELETE FROM watches WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "watch",
                id: id.to_owned(),
            });
        }
        Ok(())
    }

    async fn find_message(
        &self,
        id: MessageId,
    ) -> Result<Option<(Option<MessageId>, String)>, CoreError> {
        let row: Option<(Option<String>, String)> = sqlx::query_as(
            "SELECT json_extract(data, '$.thread'), json_extract(data, '$.text') FROM events
             WHERE type = 'agent.input' AND json_extract(data, '$.message_id') = ?1
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        Ok(row.map(|(thread, text)| (thread.and_then(|t| t.parse().ok()), text)))
    }

    async fn last_session_id(&self, agent: AgentId) -> Result<Option<String>, CoreError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT json_extract(data, '$.session_id') FROM events
             WHERE type = 'agent.session_started' AND json_extract(data, '$.id') = ?1
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(agent.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        Ok(row.map(|(id,)| id))
    }

    async fn agent_events(
        &self,
        agent: AgentId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<Event>, CoreError> {
        // Mirrors `EventKind::agent_id`.
        sqlx::query(
            "SELECT * FROM events WHERE seq > ?1 AND (
                (type LIKE 'agent.%' AND type NOT IN ('agent.created', 'agent.updated') AND json_extract(data, '$.id') = ?2)
                OR (type IN ('agent.created', 'agent.updated') AND json_extract(data, '$.agent.id') = ?2)
                OR (type = 'task.status_changed' AND json_extract(data, '$.assignee') = ?2)
            ) ORDER BY seq LIMIT ?3",
        )
        .bind(after as i64)
        .bind(agent.to_string())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .iter()
        .map(event_from_row)
        .collect()
    }

    async fn insert_run(&self, run: &Run) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO runs (id, status, created_at, updated_at, data) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(run.id.to_string())
        .bind(enum_str(&run.status)?)
        .bind(run.created_at.to_rfc3339())
        .bind(run.updated_at.to_rfc3339())
        .bind(serde_json::to_string(run).map_err(storage)?)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn get_run(&self, id: RunId) -> Result<Run, CoreError> {
        let data: Option<String> = sqlx::query_scalar("SELECT data FROM runs WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        let data = data.ok_or_else(|| CoreError::NotFound {
            kind: "run",
            id: id.to_string(),
        })?;
        serde_json::from_str(&data).map_err(storage)
    }

    async fn list_runs(&self) -> Result<Vec<Run>, CoreError> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT data FROM runs ORDER BY created_at DESC, id DESC")
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn update_run(&self, run: &Run) -> Result<(), CoreError> {
        let result =
            sqlx::query("UPDATE runs SET status = ?, updated_at = ?, data = ? WHERE id = ?")
                .bind(enum_str(&run.status)?)
                .bind(run.updated_at.to_rfc3339())
                .bind(serde_json::to_string(run).map_err(storage)?)
                .bind(run.id.to_string())
                .execute(&self.pool)
                .await
                .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "run",
                id: run.id.to_string(),
            });
        }
        Ok(())
    }

    async fn insert_task(&self, task: &Task) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO tasks (id, run_id, status, created_at, updated_at, data)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(task.id.to_string())
        .bind(task.run_id.to_string())
        .bind(enum_str(&task.status)?)
        .bind(task.created_at.to_rfc3339())
        .bind(task.updated_at.to_rfc3339())
        .bind(serde_json::to_string(task).map_err(storage)?)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn get_task(&self, id: TaskId) -> Result<Task, CoreError> {
        let data: Option<String> = sqlx::query_scalar("SELECT data FROM tasks WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        let data = data.ok_or_else(|| CoreError::NotFound {
            kind: "task",
            id: id.to_string(),
        })?;
        serde_json::from_str(&data).map_err(storage)
    }

    async fn list_tasks(&self, run_id: RunId) -> Result<Vec<Task>, CoreError> {
        // Ids are UUID v7, so ordering by id is creation order.
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT data FROM tasks WHERE run_id = ? ORDER BY id")
                .bind(run_id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn update_task(&self, task: &Task) -> Result<(), CoreError> {
        let result =
            sqlx::query("UPDATE tasks SET status = ?, updated_at = ?, data = ? WHERE id = ?")
                .bind(enum_str(&task.status)?)
                .bind(task.updated_at.to_rfc3339())
                .bind(serde_json::to_string(task).map_err(storage)?)
                .bind(task.id.to_string())
                .execute(&self.pool)
                .await
                .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "task",
                id: task.id.to_string(),
            });
        }
        Ok(())
    }

    async fn insert_account(&self, account: &Account) -> Result<(), CoreError> {
        let result = sqlx::query(
            "INSERT INTO accounts (id, provider, created_at, data) VALUES (?, ?, ?, ?)",
        )
        .bind(account.id.clone())
        .bind(account.provider.clone())
        .bind(account.created_at.to_rfc3339())
        .bind(serde_json::to_string(account).map_err(storage)?)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(CoreError::Conflict(
                format!("account {:?} already exists", account.id),
            )),
            Err(e) => Err(storage(e)),
        }
    }

    async fn get_account(&self, id: &str) -> Result<Account, CoreError> {
        let data: Option<String> = sqlx::query_scalar("SELECT data FROM accounts WHERE id = ?")
            .bind(id.to_owned())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        let data = data.ok_or_else(|| CoreError::NotFound {
            kind: "account",
            id: id.to_owned(),
        })?;
        serde_json::from_str(&data).map_err(storage)
    }

    async fn list_accounts(&self) -> Result<Vec<Account>, CoreError> {
        let rows: Vec<String> = sqlx::query_scalar("SELECT data FROM accounts ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn delete_account(&self, id: &str) -> Result<(), CoreError> {
        let result = sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(id.to_owned())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "account",
                id: id.to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crossagents_core::{Role, RunConfig, RunStatus, TaskId, TaskReport, TaskSpec, TaskStatus};

    #[tokio::test]
    async fn accounts_round_trip() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let account = Account {
            id: "codex-main".into(),
            provider: "codex".into(),
            label: "main".into(),
            home: PathBuf::from("/home/u/.codex"),
            created_at: time_from(&Utc::now().to_rfc3339()).unwrap(),
        };
        store.insert_account(&account).await.unwrap();
        assert!(matches!(
            store.insert_account(&account).await,
            Err(CoreError::Conflict(_))
        ));
        assert_eq!(store.get_account("codex-main").await.unwrap(), account);
        assert_eq!(store.list_accounts().await.unwrap(), vec![account]);
        store.delete_account("codex-main").await.unwrap();
        assert!(matches!(
            store.get_account("codex-main").await,
            Err(CoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn runs_and_tasks_round_trip() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let now = time_from(&Utc::now().to_rfc3339()).unwrap();
        let mut run = Run {
            id: RunId::new(),
            title: "t".into(),
            request: "do things".into(),
            planner: AgentId::new(),
            status: RunStatus::Planning,
            config: RunConfig {
                worker: AgentId::new(),
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: 3,
                max_attempts: 3,
                team: vec![],
            },
            repo: PathBuf::from("/repo"),
            base_branch: "main".into(),
            branch: "crossagents/x".into(),
            note: None,
            plan: vec![],
            created_at: now,
            updated_at: now,
        };
        store.insert_run(&run).await.unwrap();
        run.status = RunStatus::Running;
        store.update_run(&run).await.unwrap();
        assert_eq!(store.get_run(run.id).await.unwrap(), run);
        assert_eq!(store.list_runs().await.unwrap(), vec![run.clone()]);

        let mut tasks = Vec::new();
        for key in ["a", "b"] {
            let task = Task {
                id: TaskId::new(),
                run_id: run.id,
                key: key.into(),
                title: key.into(),
                spec: TaskSpec {
                    goal: "g".into(),
                    ..Default::default()
                },
                depends_on: vec![],
                assignee: None,
                reviewer: None,
                worker_template: None,
                reviewer_template: None,
                status: TaskStatus::Pending,
                attempts: 0,
                escalated: false,
                nudges: 0,
                branch: None,
                worktree: None,
                report: TaskReport::default(),
                created_at: now,
                updated_at: now,
            };
            store.insert_task(&task).await.unwrap();
            tasks.push(task);
        }
        tasks[1].status = TaskStatus::Running;
        store.update_task(&tasks[1]).await.unwrap();
        assert_eq!(store.list_tasks(run.id).await.unwrap(), tasks);
        assert_eq!(store.get_task(tasks[1].id).await.unwrap(), tasks[1]);
        assert!(matches!(
            store.get_task(TaskId::new()).await,
            Err(CoreError::NotFound { .. })
        ));
    }

    use super::*;

    fn agent(name: &str) -> Agent {
        Agent {
            id: AgentId::new(),
            name: name.into(),
            plugin: "claude".into(),
            driver: None,
            model: Some("haiku".into()),
            role: Role::Worker,
            cwd: PathBuf::from("/tmp/work"),
            config: serde_json::json!({ "k": 1 }),
            status: AgentStatus::Stopped,
            created_at: time_from(&Utc::now().to_rfc3339()).unwrap(),
            profile: AgentProfile {
                display_name: Some("Ada".into()),
                title: Some("设计师".into()),
                ..AgentProfile::default()
            },
        }
    }

    #[tokio::test]
    async fn agent_crud() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let alice = agent("alice");
        store.insert_agent(&alice).await.unwrap();
        store.insert_agent(&agent("bob")).await.unwrap();

        assert_eq!(
            store.get_agent(&AgentKey::Id(alice.id)).await.unwrap(),
            alice
        );
        assert_eq!(
            store
                .get_agent(&AgentKey::Name("alice".into()))
                .await
                .unwrap(),
            alice
        );
        let names: Vec<_> = store
            .list_agents()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, ["alice", "bob"]);

        let card = AgentProfile {
            avatar: Some("🎨".into()),
            color: Some("rose".into()),
            persona: Some("Loves whitespace.".into()),
            ..alice.profile.clone()
        };
        store.update_agent_profile(alice.id, &card).await.unwrap();
        let carded = store.get_agent(&AgentKey::Id(alice.id)).await.unwrap();
        assert_eq!(carded.profile, card);
        assert!(matches!(
            store.update_agent_profile(AgentId::new(), &card).await,
            Err(CoreError::NotFound { .. })
        ));

        store
            .update_agent_status(alice.id, AgentStatus::Idle)
            .await
            .unwrap();
        let updated = store.get_agent(&AgentKey::Id(alice.id)).await.unwrap();
        assert_eq!(updated.status, AgentStatus::Idle);

        store.delete_agent(alice.id).await.unwrap();
        assert!(matches!(
            store.get_agent(&AgentKey::Id(alice.id)).await,
            Err(CoreError::NotFound { .. })
        ));
        assert!(matches!(
            store.delete_agent(alice.id).await,
            Err(CoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn duplicate_name_conflicts() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        store.insert_agent(&agent("alice")).await.unwrap();
        assert!(matches!(
            store.insert_agent(&agent("alice")).await,
            Err(CoreError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn events_are_sequenced_and_paged() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let mut seqs = Vec::new();
        for i in 0..5 {
            let kind = if i % 2 == 0 {
                EventKind::HubStopping
            } else {
                EventKind::HubStarted {
                    version: i.to_string(),
                }
            };
            seqs.push(store.append_event(kind).await.unwrap().seq);
        }
        assert!(seqs.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(store.latest_event_seq().await.unwrap(), seqs[4]);

        let page = store.events_since(seqs[1], 2).await.unwrap();
        assert_eq!(page.iter().map(|e| e.seq).collect::<Vec<_>>(), &seqs[2..4]);
        assert_eq!(page[0].kind, EventKind::HubStopping);
        assert_eq!(
            page[1].kind,
            EventKind::HubStarted {
                version: "3".into()
            }
        );
    }

    #[tokio::test]
    async fn data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let alice = agent("alice");
        {
            let store = SqliteStore::open(&path).await.unwrap();
            store.insert_agent(&alice).await.unwrap();
            store
                .append_event(EventKind::AgentCreated {
                    agent: alice.clone(),
                })
                .await
                .unwrap();
            store.close().await;
        }
        let store = SqliteStore::open(&path).await.unwrap();
        assert_eq!(store.list_agents().await.unwrap(), vec![alice.clone()]);
        let events = store.events_since(0, 10).await.unwrap();
        assert_eq!(events[0].kind, EventKind::AgentCreated { agent: alice });
        store.close().await;
    }
}
