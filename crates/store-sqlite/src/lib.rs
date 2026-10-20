//! SQLite implementation of [`crossagents_core::Store`].

use std::path::{Path, PathBuf};
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, CoreError, EVENT_SCHEMA_VERSION, Event, EventKind, Store,
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
            "INSERT INTO agents (id, name, plugin, driver, model, role, cwd, config, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
}

#[cfg(test)]
mod tests {
    use crossagents_core::Role;

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
