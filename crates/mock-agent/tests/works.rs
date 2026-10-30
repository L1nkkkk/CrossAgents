//! Work groups through the real hub and SQLite.
mod common;
use common::TestHub;
use crossagents_core::Store;
use serde_json::{Value, json};

#[tokio::test]
async fn works_rounds_and_conflict() {
    let t = TestHub::start().await;
    t.add_agent_in("Pia", None, "planner", t.home.path()).await;
    let response = t
        .post("/v1/works", json!({"title":"topic","lead":"Pia"}))
        .await;
    assert_eq!(response.status(), 201);
    let work: Value = response.json().await.unwrap();
    let id = work["id"].as_str().unwrap();
    let url = format!("/v1/works/{id}/runs");
    let response = t.post(&url, json!({"request":"first"})).await;
    if response.status() != 201 {
        panic!("{}", response.text().await.unwrap());
    }
    let first: Value = response.json().await.unwrap();
    assert_eq!(first["work_id"], id);
    let response = t.post(&url, json!({"request":"second"})).await;
    assert_eq!(response.status(), 409);
    let error = response.text().await.unwrap();
    assert!(error.contains(first["id"].as_str().unwrap()), "{error}");
    let list: Value = t.get("/v1/works").await.json().await.unwrap();
    assert_eq!(list["works"][0]["id"], id);
    let detail: Value = t
        .get(&format!("/v1/works/{id}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(detail["work"]["title"], "topic");
    assert_eq!(detail["runs"][0]["id"], first["id"]);
    let cancel = t
        .post(
            &format!("/v1/runs/{}/cancel", first["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert!(
        cancel.status().is_success(),
        "{}",
        cancel.text().await.unwrap()
    );
    let second = t.post(&url, json!({"request":"second"})).await;
    assert_eq!(second.status(), 201, "{}", second.text().await.unwrap());
}

#[tokio::test]
async fn upgrades_old_run_and_migration_is_repeatable() {
    let t = TestHub::start().await;
    t.add_agent_in("Pia", None, "planner", t.home.path()).await;
    let response = t
        .post("/v1/runs", json!({"planner":"Pia","request":"old"}))
        .await;
    if response.status() != 201 {
        panic!("{}", response.text().await.unwrap());
    }
    let run: Value = response.json().await.unwrap();
    let id = run["id"].as_str().unwrap().to_owned();
    let home = t.restart_home().await;
    let paths = crossagents_hub::Paths::new(home.path());
    let store = crossagents_store_sqlite::SqliteStore::open(&paths.db())
        .await
        .unwrap();
    crossagents_store_sqlite::sqlx::query(
        "UPDATE runs SET data = json_remove(data, '$.work_id') WHERE id = ?",
    )
    .bind(&id)
    .execute(store.pool())
    .await
    .unwrap();
    crossagents_store_sqlite::sqlx::query("DROP INDEX runs_one_open_per_work")
        .execute(store.pool())
        .await
        .unwrap();
    crossagents_store_sqlite::sqlx::query("DROP TABLE works")
        .execute(store.pool())
        .await
        .unwrap();
    crossagents_store_sqlite::sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 11")
        .execute(store.pool())
        .await
        .unwrap();
    store.close().await;
    let upgraded = crossagents_store_sqlite::SqliteStore::open(&paths.db())
        .await
        .unwrap();
    assert_eq!(
        upgraded.get_run(id.parse().unwrap()).await.unwrap().work_id,
        None
    );
    crossagents_store_sqlite::sqlx::raw_sql(include_str!(
        "../../store-sqlite/migrations/0011_works.sql"
    ))
    .execute(upgraded.pool())
    .await
    .unwrap();
    assert_eq!(
        upgraded.get_run(id.parse().unwrap()).await.unwrap().work_id,
        None
    );
    upgraded.close().await;
    let t = TestHub::start_in(home, |_| {}).await;
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(detail["run"]["id"], id);
    assert!(detail["run"]["work_id"].is_null());
}
