//! SQLite implementation of [`crossagents_core::Store`].

#[doc(hidden)]
pub use sqlx;

use std::path::{Path, PathBuf};
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Account, Agent, AgentClaim, AgentId, AgentKey, AgentProfile, AgentStatus, CoreError,
    EVENT_SCHEMA_VERSION, Event, EventKind, HumanItem, IssueRef, Lesson, LessonStatus, MessageId,
    NameSource, Note, NoteScope, Project, Reminder, Run, RunId, Store, Task, TaskId, Triage,
    TriageRecord, TriageStep, Watch,
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
    /// Runs `sql` (`UPDATE agents SET <column> = ? WHERE id = ?`) for one agent.
    async fn update_column(
        &self,
        id: AgentId,
        sql: &'static str,
        value: Option<String>,
    ) -> Result<(), CoreError> {
        let result = sqlx::query(sql)
            .bind(value)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(not_found(&AgentKey::Id(id)));
        }
        Ok(())
    }

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
        MIGRATOR.run(&pool).await.map_err(migration_failed)?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// A migration that failed because the database carries one this build does not know: the file
/// was written by a newer CrossAgents (a dev hub, or an upgraded install). Said plainly, because
/// `hub start` runs the hub with stderr discarded: this sentence is all the user gets in
/// `hub.log` and on the GUI's offline page.
fn migration_failed(err: sqlx::migrate::MigrateError) -> CoreError {
    match err {
        sqlx::migrate::MigrateError::VersionMissing(version) => CoreError::Storage(format!(
            "the database comes from a newer CrossAgents (it has migration {version}, this build \
             knows up to {}); upgrade CrossAgents to open it",
            known_migration()
        )),
        other => storage(other),
    }
}

/// The highest migration this build ships.
fn known_migration() -> i64 {
    MIGRATOR
        .migrations
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap_or(0)
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
        name_source: enum_from(&get("name_source")?)?,
        plugin: get("plugin")?,
        driver: get_opt("driver")?,
        model: get_opt("model")?,
        role: enum_from(&get("role")?)?,
        // NULL only in a row written by something that predates the column.
        home: PathBuf::from(get_opt("home")?.map_or_else(|| get("cwd"), Ok)?),
        cwd: PathBuf::from(get("cwd")?),
        config: serde_json::from_str(&get("config")?).map_err(storage)?,
        status: enum_from(&get("status")?)?,
        created_at: time_from(&get("created_at")?)?,
        profile: serde_json::from_str(&get("profile")?).map_err(storage)?,
        topic: get_opt("topic")?,
        archived_at: get_opt("archived_at")?
            .as_deref()
            .map(time_from)
            .transpose()?,
    })
}

fn claim_from_row(row: &SqliteRow) -> Result<AgentClaim, CoreError> {
    let get = |col: &str| row.try_get::<String, _>(col).map_err(storage);
    Ok(AgentClaim {
        agent_id: get("agent_id")?.parse().map_err(storage)?,
        run_id: get("run_id")?.parse().map_err(storage)?,
        task_id: row
            .try_get::<Option<String>, _>("task_id")
            .map_err(storage)?
            .map(|t| t.parse().map_err(storage))
            .transpose()?,
        role: enum_from(&get("role")?)?,
        fresh: row.try_get::<bool, _>("fresh").map_err(storage)?,
        since: time_from(&get("since")?)?,
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

/// Writes a lesson and its search entry; `new` inserts, else replaces the stored one.
async fn insert_lesson(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    lesson: &Lesson,
    new: bool,
) -> Result<(), CoreError> {
    let status = match lesson.status {
        LessonStatus::Active => "active",
        LessonStatus::Review => "review",
        LessonStatus::Superseded => "superseded",
    };
    let data = serde_json::to_string(lesson).map_err(storage)?;
    let sql = if new {
        "INSERT INTO lessons (id, project_id, status, created_at, data) VALUES (?1, ?2, ?3, ?4, ?5)"
    } else {
        "UPDATE lessons SET project_id = ?2, status = ?3, created_at = ?4, data = ?5 WHERE id = ?1"
    };
    sqlx::query(sql)
        .bind(&lesson.id)
        .bind(&lesson.project_id)
        .bind(status)
        .bind(lesson.created_at.to_rfc3339())
        .bind(data)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    sqlx::query("DELETE FROM lessons_fts WHERE id = ?")
        .bind(&lesson.id)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    sqlx::query(
        "INSERT INTO lessons_fts (id, title, symptom, keywords, resolution)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(&lesson.id)
    .bind(&lesson.title)
    .bind(&lesson.symptom)
    .bind(lesson.keywords.join(" "))
    .bind(&lesson.resolution)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
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
            "INSERT INTO agents (id, name, plugin, driver, model, role, cwd, config, status, created_at, profile, name_source, topic, archived_at, home)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(enum_str(&agent.name_source)?)
        .bind(&agent.topic)
        .bind(agent.archived_at.map(|t| t.to_rfc3339()))
        .bind(agent.home().to_string_lossy().into_owned())
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
                sqlx::query("SELECT * FROM agents WHERE name = ? COLLATE NOCASE").bind(name.clone())
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
        sqlx::query("DELETE FROM agent_claims WHERE agent_id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn update_agent_home(&self, id: AgentId, home: &Path) -> Result<(), CoreError> {
        self.update_column(
            id,
            "UPDATE agents SET home = ? WHERE id = ?",
            Some(home.to_string_lossy().into_owned()),
        )
        .await
    }

    async fn claim_agent(&self, claim: &AgentClaim) -> Result<Option<AgentClaim>, CoreError> {
        // One statement: the primary key decides between two runs taking the agent at once.
        let result = sqlx::query(
            "INSERT INTO agent_claims (agent_id, run_id, task_id, role, fresh, since)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(claim.agent_id.to_string())
        .bind(claim.run_id.to_string())
        .bind(claim.task_id.map(|t| t.to_string()))
        .bind(enum_str(&claim.role)?)
        .bind(claim.fresh)
        .bind(claim.since.to_rfc3339())
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(None),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                match self.claim_of(claim.agent_id).await? {
                    Some(holder) => Ok(Some(holder)),
                    // Let go between the insert and this read: say so rather than retry.
                    None => Err(CoreError::Busy(
                        "the agent was just let go by another run; try again".into(),
                    )),
                }
            }
            Err(e) => Err(storage(e)),
        }
    }

    async fn release_agent(&self, agent: AgentId, run: RunId) -> Result<bool, CoreError> {
        let result = sqlx::query("DELETE FROM agent_claims WHERE agent_id = ? AND run_id = ?")
            .bind(agent.to_string())
            .bind(run.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(result.rows_affected() > 0)
    }

    async fn claim_of(&self, agent: AgentId) -> Result<Option<AgentClaim>, CoreError> {
        sqlx::query("SELECT * FROM agent_claims WHERE agent_id = ?")
            .bind(agent.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?
            .as_ref()
            .map(claim_from_row)
            .transpose()
    }

    async fn list_claims(&self, run: Option<RunId>) -> Result<Vec<AgentClaim>, CoreError> {
        let rows = match run {
            Some(run) => {
                sqlx::query("SELECT * FROM agent_claims WHERE run_id = ? ORDER BY since, agent_id")
                    .bind(run.to_string())
                    .fetch_all(&self.pool)
                    .await
            }
            None => {
                sqlx::query("SELECT * FROM agent_claims ORDER BY since, agent_id")
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(storage)?;
        rows.iter().map(claim_from_row).collect()
    }

    async fn rename_agent(
        &self,
        id: AgentId,
        name: &str,
        source: NameSource,
    ) -> Result<(), CoreError> {
        let result = sqlx::query("UPDATE agents SET name = ?, name_source = ? WHERE id = ?")
            .bind(name)
            .bind(enum_str(&source)?)
            .bind(id.to_string())
            .execute(&self.pool)
            .await;
        match result {
            Ok(r) if r.rows_affected() == 0 => Err(not_found(&AgentKey::Id(id))),
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(CoreError::Conflict(
                format!("agent name {name:?} is already taken"),
            )),
            Err(e) => Err(storage(e)),
        }
    }

    async fn update_agent_topic(&self, id: AgentId, topic: Option<&str>) -> Result<(), CoreError> {
        self.update_column(
            id,
            "UPDATE agents SET topic = ? WHERE id = ?",
            topic.map(str::to_owned),
        )
        .await
    }

    async fn set_agent_archived(
        &self,
        id: AgentId,
        at: Option<DateTime<Utc>>,
    ) -> Result<(), CoreError> {
        self.update_column(
            id,
            "UPDATE agents SET archived_at = ? WHERE id = ?",
            at.map(|t| t.to_rfc3339()),
        )
        .await
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

    async fn update_agent_cwd(&self, id: AgentId, cwd: &std::path::Path) -> Result<(), CoreError> {
        let result = sqlx::query("UPDATE agents SET cwd = ? WHERE id = ?")
            .bind(cwd.to_string_lossy().into_owned())
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(not_found(&AgentKey::Id(id)));
        }
        Ok(())
    }

    async fn update_agent_config(
        &self,
        id: AgentId,
        config: &serde_json::Value,
    ) -> Result<(), CoreError> {
        let result = sqlx::query("UPDATE agents SET config = ? WHERE id = ?")
            .bind(serde_json::to_string(config).map_err(storage)?)
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

    async fn put_note(&self, note: &Note) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO notes (scope, run_id, key, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(scope, run_id, key) DO UPDATE SET data = ?4",
        )
        .bind(scope_name(note.scope))
        .bind(note.run_id.map(|r| r.to_string()).unwrap_or_default())
        .bind(&note.key)
        .bind(serde_json::to_string(note).map_err(storage)?)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn get_note(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        key: &str,
    ) -> Result<Option<Note>, CoreError> {
        let data: Option<String> = sqlx::query_scalar(
            "SELECT data FROM notes WHERE scope = ?1 AND run_id = ?2 AND key = ?3",
        )
        .bind(scope_name(scope))
        .bind(run_id.map(|r| r.to_string()).unwrap_or_default())
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        data.map(|d| serde_json::from_str(&d).map_err(storage))
            .transpose()
    }

    async fn list_notes(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        prefix: &str,
    ) -> Result<Vec<Note>, CoreError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT data FROM notes WHERE scope = ?1 AND run_id = ?2 AND substr(key, 1, length(?3)) = ?3 ORDER BY key",
        )
        .bind(scope_name(scope))
        .bind(run_id.map(|r| r.to_string()).unwrap_or_default())
        .bind(prefix)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn delete_note(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        key: &str,
    ) -> Result<(), CoreError> {
        sqlx::query("DELETE FROM notes WHERE scope = ?1 AND run_id = ?2 AND key = ?3")
            .bind(scope_name(scope))
            .bind(run_id.map(|r| r.to_string()).unwrap_or_default())
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn put_project(&self, project: &Project) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO projects (id, data) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = ?2",
        )
        .bind(&project.id)
        .bind(serde_json::to_string(project).map_err(storage)?)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn list_projects(&self) -> Result<Vec<Project>, CoreError> {
        let rows: Vec<String> = sqlx::query_scalar("SELECT data FROM projects ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn triage(
        &self,
        issues: &[IssueRef],
        step: TriageStep,
        by: AgentId,
    ) -> Result<(Vec<TriageRecord>, Option<Lesson>), CoreError> {
        // Write lock first, as in `delete_run`: this reads, then writes.
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let mut records = Vec::with_capacity(issues.len());
        let mut fresh: Vec<IssueRef> = Vec::new();
        for issue in issues {
            let earlier: Option<String> =
                sqlx::query_scalar("SELECT data FROM issue_triage WHERE run_id = ?1 AND key = ?2")
                    .bind(issue.run_id.to_string())
                    .bind(&issue.key)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(storage)?;
            match earlier {
                Some(data) => records.push(serde_json::from_str(&data).map_err(storage)?),
                None if !fresh.contains(issue) => fresh.push(issue.clone()),
                None => {}
            }
        }
        if fresh.is_empty() {
            // A retry of a step that went through: hand back what it did.
            tx.rollback().await.map_err(storage)?;
            let lesson = match records.first().map(|r: &TriageRecord| &r.triage) {
                Some(Triage::Lesson { lesson_id } | Triage::Merged { lesson_id }) => {
                    Some(self.get_lesson(lesson_id).await?)
                }
                _ => None,
            };
            return Ok((records, lesson));
        }
        let (triage, lesson) = match step {
            TriageStep::Create(mut lesson) => {
                lesson.sources = fresh.clone();
                insert_lesson(&mut tx, &lesson, true).await?;
                let triage = Triage::Lesson {
                    lesson_id: lesson.id.clone(),
                };
                (triage, Some(lesson))
            }
            TriageStep::Merge(mut lesson) => {
                let stored: Option<String> =
                    sqlx::query_scalar("SELECT id FROM lessons WHERE id = ?")
                        .bind(&lesson.id)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(storage)?;
                if stored.is_none() {
                    return Err(CoreError::NotFound {
                        kind: "lesson",
                        id: lesson.id,
                    });
                }
                for issue in &fresh {
                    if !lesson.sources.contains(issue) {
                        lesson.sources.push(issue.clone());
                    }
                }
                insert_lesson(&mut tx, &lesson, false).await?;
                let triage = Triage::Merged {
                    lesson_id: lesson.id.clone(),
                };
                (triage, Some(lesson))
            }
            TriageStep::Keep { why } => (Triage::Kept { why }, None),
        };
        let at = Utc::now();
        for issue in fresh {
            let record = TriageRecord {
                issue,
                triage: triage.clone(),
                by,
                at,
            };
            sqlx::query("INSERT INTO issue_triage (run_id, key, data) VALUES (?1, ?2, ?3)")
                .bind(record.issue.run_id.to_string())
                .bind(&record.issue.key)
                .bind(serde_json::to_string(&record).map_err(storage)?)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            records.push(record);
        }
        tx.commit().await.map_err(storage)?;
        Ok((records, lesson))
    }

    async fn list_triage(&self, run: RunId) -> Result<Vec<TriageRecord>, CoreError> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT data FROM issue_triage WHERE run_id = ? ORDER BY key")
                .bind(run.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn get_lesson(&self, id: &str) -> Result<Lesson, CoreError> {
        let data: Option<String> = sqlx::query_scalar("SELECT data FROM lessons WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        match data {
            Some(d) => serde_json::from_str(&d).map_err(storage),
            None => Err(CoreError::NotFound {
                kind: "lesson",
                id: id.to_owned(),
            }),
        }
    }

    async fn update_lesson(&self, lesson: &Lesson) -> Result<(), CoreError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let stored: Option<String> = sqlx::query_scalar("SELECT id FROM lessons WHERE id = ?")
            .bind(&lesson.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
        if stored.is_none() {
            return Err(CoreError::NotFound {
                kind: "lesson",
                id: lesson.id.clone(),
            });
        }
        insert_lesson(&mut tx, lesson, false).await?;
        tx.commit().await.map_err(storage)
    }

    async fn list_lessons(&self, project: &str) -> Result<Vec<Lesson>, CoreError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT data FROM lessons WHERE project_id = ? ORDER BY created_at DESC",
        )
        .bind(project)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn search_lessons(
        &self,
        project: &str,
        terms: &[String],
        superseded: bool,
        limit: usize,
    ) -> Result<Vec<Lesson>, CoreError> {
        let terms: Vec<&str> = terms
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // Trigrams need three characters; shorter terms ("代理") are looked up as substrings.
        let (long, short): (Vec<&str>, Vec<&str>) =
            terms.into_iter().partition(|t| t.chars().count() >= 3);
        let mut found: Vec<String> = Vec::new();
        if !long.is_empty() {
            let query = long
                .iter()
                .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" OR ");
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT l.data FROM lessons_fts f JOIN lessons l ON l.id = f.id
                 WHERE lessons_fts MATCH ?1 AND l.project_id = ?2
                   AND (l.status != 'superseded' OR ?4)
                 ORDER BY bm25(lessons_fts) LIMIT ?3",
            )
            .bind(query)
            .bind(project)
            .bind(limit as i64)
            .bind(superseded)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            found.extend(rows);
        }
        for term in short {
            if found.len() >= limit {
                break;
            }
            let pattern = format!(
                "%{}%",
                term.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT l.data FROM lessons_fts f JOIN lessons l ON l.id = f.id
                 WHERE l.project_id = ?1 AND (l.status != 'superseded' OR ?4)
                   AND (f.title LIKE ?2 ESCAPE '\\' OR f.symptom LIKE ?2 ESCAPE '\\'
                        OR f.keywords LIKE ?2 ESCAPE '\\' OR f.resolution LIKE ?2 ESCAPE '\\')
                 ORDER BY l.created_at DESC LIMIT ?3",
            )
            .bind(project)
            .bind(pattern)
            .bind(limit as i64)
            .bind(superseded)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            for row in rows {
                if !found.contains(&row) {
                    found.push(row);
                }
            }
        }
        found.truncate(limit);
        found
            .iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn put_human_item(&self, item: &HumanItem) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO human_items (id, status, created_at, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET status = ?2, data = ?4",
        )
        .bind(&item.id)
        .bind(
            serde_json::to_value(item.status)
                .map_err(storage)?
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
        .bind(item.created_at.to_rfc3339())
        .bind(serde_json::to_string(item).map_err(storage)?)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn get_human_item(&self, id: &str) -> Result<HumanItem, CoreError> {
        let data: Option<String> = sqlx::query_scalar("SELECT data FROM human_items WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        match data {
            Some(d) => serde_json::from_str(&d).map_err(storage),
            None => Err(CoreError::NotFound {
                kind: "question",
                id: id.to_owned(),
            }),
        }
    }

    async fn list_human_items(&self, open_only: bool) -> Result<Vec<HumanItem>, CoreError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT data FROM human_items WHERE ?1 = 0 OR status = 'open' ORDER BY created_at DESC LIMIT 500",
        )
        .bind(open_only)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
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

    async fn insert_work(&self, work: &crossagents_core::Work) -> Result<(), CoreError> {
        sqlx::query("INSERT INTO works (id, created_at, updated_at, data) VALUES (?, ?, ?, ?)")
            .bind(work.id.to_string())
            .bind(work.created_at.to_rfc3339())
            .bind(work.updated_at.to_rfc3339())
            .bind(serde_json::to_string(work).map_err(storage)?)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn get_work(
        &self,
        id: crossagents_core::WorkId,
    ) -> Result<crossagents_core::Work, CoreError> {
        let data: Option<String> = sqlx::query_scalar("SELECT data FROM works WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        let data = data.ok_or_else(|| CoreError::NotFound {
            kind: "work",
            id: id.to_string(),
        })?;
        serde_json::from_str(&data).map_err(storage)
    }

    async fn list_works(&self) -> Result<Vec<crossagents_core::Work>, CoreError> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT data FROM works ORDER BY updated_at DESC, id DESC")
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        rows.iter()
            .map(|d| serde_json::from_str(d).map_err(storage))
            .collect()
    }

    async fn update_work(&self, work: &crossagents_core::Work) -> Result<(), CoreError> {
        let result = sqlx::query("UPDATE works SET updated_at = ?, data = ? WHERE id = ?")
            .bind(work.updated_at.to_rfc3339())
            .bind(serde_json::to_string(work).map_err(storage)?)
            .bind(work.id.to_string())
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "work",
                id: work.id.to_string(),
            });
        }
        Ok(())
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

    /// Deletes the run, its tasks, and the human items and notes that name it, in one
    /// transaction (docs/tasks/R1.md §3.3). `tasks` are deleted explicitly rather than by
    /// `ON DELETE CASCADE`: a connection need not have `foreign_keys` on. Events stay.
    async fn delete_run(&self, id: RunId) -> Result<(), CoreError> {
        // Takes the write lock first: a deferred transaction that reads and then writes is
        // refused outright (SQLITE_BUSY_SNAPSHOT) when another connection wrote in between,
        // as an agent let go just before the delete does.
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM runs WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
        if exists.is_none() {
            return Err(CoreError::NotFound {
                kind: "run",
                id: id.to_string(),
            });
        }
        // human_items has no run column; the record is JSON.
        sqlx::query("DELETE FROM human_items WHERE json_extract(data, '$.run_id') = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        // How its issues were sorted goes with them; lessons made of them stay.
        sqlx::query("DELETE FROM issue_triage WHERE run_id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        // '' is a team note, so it never matches a run id.
        sqlx::query("DELETE FROM notes WHERE run_id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        // The run's own claims; another run's on the same agents are not touched.
        sqlx::query("DELETE FROM agent_claims WHERE run_id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM tasks WHERE run_id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM runs WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
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

fn scope_name(scope: NoteScope) -> &'static str {
    match scope {
        NoteScope::Run => "run",
        NoteScope::Team => "team",
    }
}

#[cfg(test)]
mod tests {
    use crossagents_core::{
        ClaimRole, Role, RunConfig, RunStatus, TaskId, TaskReport, TaskSpec, TaskStatus,
    };

    #[tokio::test]
    async fn accounts_round_trip() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let account = Account {
            codebuddy_site: None,
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
            origin_chat_id: Some(AgentId::new()),
            work_id: None,
            continuation: None,
            landed: None,
            lead_note: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
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
                policy: Default::default(),
            },
            repo: PathBuf::from("/repo"),
            base_branch: "main".into(),
            branch: "crossagents/x".into(),
            in_place: false,
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

        // Older JSON has no source field; no schema migration is required.
        let mut old = serde_json::to_value(&run).unwrap();
        old.as_object_mut().unwrap().remove("origin_chat_id");
        sqlx::query("UPDATE runs SET data = ? WHERE id = ?")
            .bind(old.to_string())
            .bind(run.id.to_string())
            .execute(store.pool())
            .await
            .unwrap();
        assert_eq!(store.get_run(run.id).await.unwrap().origin_chat_id, None);

        let mut tasks = Vec::new();
        for key in ["a", "b"] {
            let task = Task {
                issuer: None,
                progress: None,
                handoffs: Vec::new(),
                parent: None,
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
                worker_fresh: true,
                worker_restore: false,
                reviewer_fresh: true,
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

    /// docs/tasks/R1.md §3.4: what the run owned goes with it; the log and other runs' things
    /// stay; deleting it again says it is not there.
    #[tokio::test]
    async fn deleting_a_run_takes_its_tasks_items_and_notes() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let now = time_from(&Utc::now().to_rfc3339()).unwrap();
        let run = Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            landed: None,
            lead_note: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
            id: RunId::new(),
            title: "t".into(),
            request: "do things".into(),
            planner: AgentId::new(),
            status: RunStatus::Completed,
            config: RunConfig {
                worker: AgentId::new(),
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: 3,
                max_attempts: 3,
                team: vec![],
                policy: Default::default(),
            },
            repo: PathBuf::from("/repo"),
            base_branch: "main".into(),
            branch: "crossagents/x".into(),
            in_place: false,
            note: None,
            plan: vec![],
            created_at: now,
            updated_at: now,
        };
        store.insert_run(&run).await.unwrap();
        let task = Task {
            issuer: None,
            progress: None,
            handoffs: Vec::new(),
            parent: None,
            id: TaskId::new(),
            run_id: run.id,
            key: "a".into(),
            title: "a".into(),
            spec: TaskSpec {
                goal: "g".into(),
                ..Default::default()
            },
            depends_on: vec![],
            assignee: None,
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Merged,
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
        let item = |id: &str, run_id: Option<RunId>| HumanItem {
            id: id.into(),
            kind: crossagents_core::HumanItemKind::Notice,
            from: run.planner,
            run_id,
            task_id: None,
            text: id.into(),
            detail: None,
            options: vec![],
            default: None,
            timeout_at: None,
            status: crossagents_core::HumanItemStatus::Open,
            answer: None,
            answered_at: None,
            action: None,
            created_at: now,
        };
        store
            .put_human_item(&item("about-the-run", Some(run.id)))
            .await
            .unwrap();
        store
            .put_human_item(&item("about-something-else", None))
            .await
            .unwrap();
        let note = |scope: NoteScope, run_id: Option<RunId>, key: &str| Note {
            scope,
            run_id,
            key: key.into(),
            text: key.into(),
            author: run.planner,
            updated_at: now,
        };
        store
            .put_note(&note(NoteScope::Run, Some(run.id), "progress"))
            .await
            .unwrap();
        store
            .put_note(&note(NoteScope::Team, None, "conventions"))
            .await
            .unwrap();

        store.delete_run(run.id).await.unwrap();

        assert!(matches!(
            store.get_run(run.id).await,
            Err(CoreError::NotFound { .. })
        ));
        assert!(store.list_tasks(run.id).await.unwrap().is_empty());
        let items = store.list_human_items(true).await.unwrap();
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["about-something-else"],
            "{items:?}"
        );
        assert!(
            store
                .list_notes(NoteScope::Run, Some(run.id), "")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .list_notes(NoteScope::Team, None, "")
                .await
                .unwrap()
                .len(),
            1,
            "a team note is not the run's"
        );
        // A run that is not there any more: NotFound, not a silent success.
        assert!(matches!(
            store.delete_run(run.id).await,
            Err(CoreError::NotFound { .. })
        ));
    }

    use super::*;

    fn agent(name: &str) -> Agent {
        Agent {
            id: AgentId::new(),
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            home: PathBuf::from("/tmp/work"),
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
    async fn one_claim_per_agent_and_runs_only_release_their_own() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let rex = AgentId::new();
        let (run_a, run_b) = (RunId::new(), RunId::new());
        let task = TaskId::new();
        let held = AgentClaim::new(rex, run_a, Some(task), ClaimRole::Worker, false);
        assert_eq!(store.claim_agent(&held).await.unwrap(), None);
        // Another run cannot take it, and learns who has it.
        let other = AgentClaim::new(rex, run_b, None, ClaimRole::Lead, false);
        let holder = store.claim_agent(&other).await.unwrap().unwrap();
        assert_eq!(holder.run_id, run_a);
        assert_eq!(holder.task_id, Some(task));
        assert_eq!(holder.role, ClaimRole::Worker);
        // Run B letting go of it touches nothing: the row is run A's.
        assert!(!store.release_agent(rex, run_b).await.unwrap());
        assert_eq!(store.claim_of(rex).await.unwrap().unwrap().run_id, run_a);
        assert_eq!(store.list_claims(Some(run_b)).await.unwrap(), vec![]);
        assert!(store.release_agent(rex, run_a).await.unwrap());
        assert_eq!(store.claim_of(rex).await.unwrap(), None);
        assert_eq!(store.claim_agent(&other).await.unwrap(), None);
        assert_eq!(store.list_claims(Some(run_b)).await.unwrap(), vec![other]);
    }

    #[tokio::test]
    async fn two_runs_claiming_at_once_only_one_wins() {
        let dir = tempfile::tempdir().unwrap();
        // A file, so the two claims go over separate connections.
        let store = SqliteStore::open(&dir.path().join("state.db"))
            .await
            .unwrap();
        let rex = AgentId::new();
        let (a, b) = (
            AgentClaim::new(rex, RunId::new(), None, ClaimRole::Worker, false),
            AgentClaim::new(rex, RunId::new(), None, ClaimRole::Worker, false),
        );
        let (ra, rb) = tokio::join!(store.claim_agent(&a), store.claim_agent(&b));
        let (ra, rb) = (ra.unwrap(), rb.unwrap());
        assert!(ra.is_none() ^ rb.is_none(), "{ra:?} {rb:?}");
        let winner = if ra.is_none() { &a } else { &b };
        assert_eq!(ra.or(rb).unwrap().run_id, winner.run_id);
        store.close().await;
    }

    #[tokio::test]
    async fn agents_from_before_homes_get_their_cwd_as_home() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .unwrap();
        MIGRATOR.run_to(8, &pool).await.unwrap();
        let id = AgentId::new();
        sqlx::query(
            "INSERT INTO agents (id, name, plugin, role, cwd, status, created_at, profile)
             VALUES (?, 'old', 'claude', 'worker', '/w/repo', 'stopped', ?, '{}')",
        )
        .bind(id.to_string())
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        // The next hub opens the database it left and migrates it.
        let store = SqliteStore::open(&path).await.unwrap();
        let old = store.get_agent(&AgentKey::Id(id)).await.unwrap();
        assert_eq!(old.home, PathBuf::from("/w/repo"));
        store
            .update_agent_home(id, Path::new("/w/other"))
            .await
            .unwrap();
        let old = store.get_agent(&AgentKey::Id(id)).await.unwrap();
        assert_eq!((old.home, old.cwd), ("/w/other".into(), "/w/repo".into()));
        store.close().await;
    }

    /// docs/TODO.md §一: a database written by a newer build says so, instead of refusing to
    /// open with "migration 11 was previously applied but is missing".
    #[tokio::test]
    async fn a_database_from_a_newer_build_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        // A migration this build does not ship, as a newer hub leaves behind.
        {
            let options = SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true);
            let pool = SqlitePoolOptions::new()
                .connect_with(options)
                .await
                .unwrap();
            MIGRATOR.run(&pool).await.unwrap();
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
                 VALUES (?, 'from a newer build', TRUE, X'00', 0)",
            )
            .bind(known_migration() + 1)
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        }
        let err = match SqliteStore::open(&path).await {
            Ok(_) => panic!("a database from a newer build must not open"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("comes from a newer CrossAgents"), "{err}");
        assert!(err.contains("upgrade"), "{err}");
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

    fn run_for(id: RunId) -> Run {
        let now = Utc::now();
        Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            landed: None,
            lead_note: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
            id,
            title: "t".into(),
            request: "do things".into(),
            planner: AgentId::new(),
            status: RunStatus::Completed,
            config: RunConfig {
                worker: AgentId::new(),
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: 3,
                max_attempts: 3,
                team: vec![],
                policy: Default::default(),
            },
            repo: PathBuf::from("/repo"),
            base_branch: "main".into(),
            branch: "crossagents/x".into(),
            in_place: false,
            note: None,
            plan: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    fn lesson(id: &str, project: &str, title: &str, symptom: &str) -> Lesson {
        let now = Utc::now();
        Lesson {
            id: id.into(),
            project_id: project.into(),
            title: title.into(),
            symptom: symptom.into(),
            keywords: Vec::new(),
            conditions: String::new(),
            resolution: "use node".into(),
            outcome: crossagents_core::LessonOutcome::Workaround,
            status: LessonStatus::Active,
            superseded_by: None,
            evidence: Vec::new(),
            sources: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Sorting is one transaction and safe to repeat: a retry writes nothing new, and only
    /// the issues not sorted yet go into a step (docs/tasks/LESSONS.md §4.3).
    #[tokio::test]
    async fn sorting_issues_twice_stores_the_lesson_once() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let run = RunId::new();
        let by = AgentId::new();
        let issue = |k: &str| IssueRef {
            run_id: run,
            key: format!("issue/t/{k}"),
        };
        let step = || TriageStep::Create(lesson("l1", "p", "python is a stub", "exit 9009"));
        let (records, made) = store
            .triage(&[issue("a"), issue("b")], step(), by)
            .await
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(made.unwrap().sources, vec![issue("a"), issue("b")]);
        // The same call again, as after a crash: the same answer, nothing added.
        let (again, made) = store
            .triage(&[issue("a"), issue("b")], step(), by)
            .await
            .unwrap();
        assert_eq!(again, records);
        assert_eq!(made.unwrap().id, "l1");
        assert_eq!(store.list_lessons("p").await.unwrap().len(), 1);
        // A step over one sorted and one new issue only takes the new one.
        let (mixed, _) = store
            .triage(
                &[issue("a"), issue("c")],
                TriageStep::Keep {
                    why: "one-off".into(),
                },
                by,
            )
            .await
            .unwrap();
        assert!(matches!(mixed[0].triage, Triage::Lesson { .. }));
        assert!(matches!(mixed[1].triage, Triage::Kept { .. }));
        assert_eq!(store.list_triage(run).await.unwrap().len(), 3);
        // Merging into a lesson that is not there writes nothing.
        let missing = store
            .triage(
                &[issue("d")],
                TriageStep::Merge(lesson("nope", "p", "x", "x")),
                by,
            )
            .await;
        assert!(matches!(missing, Err(CoreError::NotFound { .. })));
        assert_eq!(store.list_triage(run).await.unwrap().len(), 3);
        // Deleting the run takes its sorting records; the lesson stays.
        store.insert_run(&run_for(run)).await.unwrap();
        store.delete_run(run).await.unwrap();
        assert!(store.list_triage(run).await.unwrap().is_empty());
        assert_eq!(store.list_lessons("p").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn lessons_are_searched_within_their_project() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let by = AgentId::new();
        let run = RunId::new();
        let mut python = lesson(
            "l1",
            "p",
            "python 是 Windows Store 的占位程序",
            "python exited with 9009",
        );
        python.keywords = vec!["9009".into(), "WindowsApps".into()];
        let proxy = lesson("l2", "p", "claude.com 要走代理", "connect ETIMEDOUT");
        let other = lesson("l3", "q", "python stub elsewhere", "9009");
        for (i, l) in [python, proxy, other].into_iter().enumerate() {
            let issue = IssueRef {
                run_id: run,
                key: format!("issue/t/{i}"),
            };
            store
                .triage(&[issue], TriageStep::Create(l), by)
                .await
                .unwrap();
        }
        let ids = |ls: Vec<Lesson>| ls.into_iter().map(|l| l.id).collect::<Vec<_>>();
        let search = |terms: &[&str], superseded: bool| {
            let terms: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
            let store = &store;
            async move {
                ids(store
                    .search_lessons("p", &terms, superseded, 5)
                    .await
                    .unwrap())
            }
        };
        // Inside longer words, either language; another project's lesson never shows.
        assert_eq!(search(&["9009"], false).await, vec!["l1"]);
        assert_eq!(search(&["占位程序"], false).await, vec!["l1"]);
        assert_eq!(search(&["TIMEDOUT"], false).await, vec!["l2"]);
        // Shorter than a trigram: looked up as a substring.
        assert_eq!(search(&["代理"], false).await, vec!["l2"]);
        assert!(search(&["nothing like it"], false).await.is_empty());
        // Quotes and FTS syntax in what an agent pastes are taken as text.
        assert!(
            search(&["\"; DROP TABLE lessons; --"], false)
                .await
                .is_empty()
        );
        assert!(search(&["9009 OR NEAR(x"], false).await.is_empty());
        // Superseded lessons only when asked for.
        let mut old = store.get_lesson("l1").await.unwrap();
        old.status = LessonStatus::Superseded;
        store.update_lesson(&old).await.unwrap();
        assert!(search(&["9009"], false).await.is_empty());
        assert_eq!(search(&["9009"], true).await, vec!["l1"]);
    }
}
