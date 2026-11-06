//! Integration tests of the read-only Git view (docs/TODO.md §14) against a real hub on a
//! random port: branches with their upstreams, the working copy status, one commit's
//! details, the whole repository's graph, and the refusal of paths the hub does not know.

use std::path::Path;

use crossagents_api_types::{
    GitBranchesResponse, GitCommitResponse, GitFileState, GitGraph, GitRefKind, GitStatusResponse,
    Run,
};
use crossagents_hub::{HubConfig, Paths, RunningHub};
use reqwest::{Client, StatusCode};
use serde_json::json;

struct TestHub {
    hub: RunningHub,
    client: Client,
    base: String,
    _home: tempfile::TempDir,
}

impl TestHub {
    async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// A hub whose configuration is adjusted before it starts (a plugin's CLI, say).
    async fn start_with(adjust: impl FnOnce(&mut HubConfig)) -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let mut config = HubConfig::new(paths.clone(), 0);
        adjust(&mut config);
        let hub = crossagents_hub::start(config).await.unwrap();
        let base = format!("http://{}", hub.addr);
        Self {
            hub,
            client: Client::new(),
            base,
            _home: home,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.get(self.url(path)).bearer_auth(&self.hub.token)
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.url(path))
            .bearer_auth(&self.hub.token)
    }

    /// `GET route?path=` for one project directory.
    fn git_get(&self, route: &str, path: &Path) -> reqwest::RequestBuilder {
        self.get(&format!(
            "{route}?path={}",
            query_escape(&path.to_string_lossy())
        ))
    }

    async fn create_agent(&self, name: &str, cwd: &Path) {
        let created = self
            .post("/v1/agents")
            .json(&json!({
                "name": name, "plugin": "claude", "role": "worker",
                "cwd": cwd.display().to_string(),
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            created.status(),
            StatusCode::CREATED,
            "{}",
            created.text().await.unwrap()
        );
    }
}

/// Runs git in `dir`, returning its trimmed stdout; panics when git says no.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A query-string value: everything outside the unreserved set, percent-encoded.
fn query_escape(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// A repository with one commit on `main`, and that commit's sha.
fn repository() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    let sha = commit(dir.path(), "one");
    (dir, sha)
}

/// Commits everything in `dir` as `message`, returning the commit's sha.
fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
    git(dir, &["rev-parse", "HEAD"])
}

/// A bare repository, and a clone of it with one commit pushed: `(origin, clone)`.
fn origin_with_a_clone() -> (tempfile::TempDir, tempfile::TempDir) {
    let origin = tempfile::tempdir().unwrap();
    git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
    let local = tempfile::tempdir().unwrap();
    git(
        local.path(),
        &["clone", "-q", origin.path().to_str().unwrap(), "."],
    );
    std::fs::write(local.path().join("a.txt"), "one\n").unwrap();
    commit(local.path(), "one");
    git(local.path(), &["push", "-q", "-u", "origin", "main"]);
    (origin, local)
}

// ---------------------------------------------------------------------------------------
// Branches

#[tokio::test]
async fn branches_carry_upstreams_and_lead_or_lag() {
    let t = TestHub::start().await;
    let (origin, other) = origin_with_a_clone();
    // A clone of the same origin, one commit ahead of the other checkout.
    let repo = tempfile::tempdir().unwrap();
    git(
        repo.path(),
        &["clone", "-q", origin.path().to_str().unwrap(), "."],
    );
    git(repo.path(), &["checkout", "-q", "-b", "topic"]);
    git(repo.path(), &["checkout", "-q", "main"]);

    // origin moves one commit ahead; the checkout then adds one of its own.
    std::fs::write(other.path().join("b.txt"), "two\n").unwrap();
    commit(other.path(), "two");
    git(other.path(), &["push", "-q", "origin", "main"]);
    // The checkout learns of it (fetching is the test's doing, not the API's), then adds
    // one of its own, so it is both ahead and behind.
    git(repo.path(), &["fetch", "-q", "origin"]);
    std::fs::write(repo.path().join("c.txt"), "three\n").unwrap();
    let head = commit(repo.path(), "three");
    let origin_head = git(other.path(), &["rev-parse", "HEAD"]);

    t.create_agent("watcher", repo.path()).await;
    let branches: GitBranchesResponse = t
        .git_get("/v1/git/branches", repo.path())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(!branches.root.is_empty());
    assert_eq!(branches.head.as_deref(), Some("main"));
    assert_eq!(branches.head_sha.as_deref(), Some(head.as_str()));

    let main = branches
        .branches
        .iter()
        .find(|branch| branch.name == "main")
        .expect("main is listed");
    assert!(!main.remote);
    assert!(main.current);
    assert_eq!(main.sha, head);
    assert_eq!(main.upstream.as_deref(), Some("origin/main"));
    assert_eq!((main.ahead, main.behind), (1, 1), "{main:?}");

    let remote = branches
        .branches
        .iter()
        .find(|branch| branch.name == "origin/main")
        .expect("origin/main is listed");
    assert!(remote.remote);
    assert!(!remote.current);
    assert_eq!(remote.sha, origin_head);
    assert_eq!(remote.upstream, None);
    // A clone's `origin/HEAD` is a symbolic ref, not a branch of its own.
    assert!(
        !branches
            .branches
            .iter()
            .any(|branch| branch.name.ends_with("/HEAD")),
        "{:?}",
        branches.branches
    );

    // A branch nobody tracks has no upstream and no lead or lag.
    let topic = branches
        .branches
        .iter()
        .find(|branch| branch.name == "topic")
        .expect("topic is listed");
    assert_eq!(topic.upstream, None);
    assert_eq!((topic.ahead, topic.behind), (0, 0));
}

// ---------------------------------------------------------------------------------------
// Status

#[tokio::test]
async fn status_parses_porcelain_v2() {
    let t = TestHub::start().await;
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    commit(repo.path(), "one");
    std::fs::write(repo.path().join("gone.txt"), "bye\n").unwrap();
    let head = commit(repo.path(), "two");
    git(repo.path(), &["mv", "gone.txt", "moved.txt"]);

    // Modified in the working tree, untracked, and a staged rename.
    std::fs::write(repo.path().join("a.txt"), "one changed\n").unwrap();
    std::fs::write(repo.path().join("new.txt"), "new\n").unwrap();

    t.create_agent("watcher", repo.path()).await;
    let status: GitStatusResponse = t
        .git_get("/v1/git/status", repo.path())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(status.head.as_deref(), Some(head.as_str()));
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.upstream, None);
    assert_eq!((status.ahead, status.behind), (0, 0));

    let modified = status
        .entries
        .iter()
        .find(|entry| entry.path == "a.txt")
        .expect("a.txt is listed");
    assert_eq!(modified.index, GitFileState::Unchanged);
    assert_eq!(modified.worktree, GitFileState::Modified);
    assert!(!modified.untracked && !modified.conflicted);

    let untracked = status
        .entries
        .iter()
        .find(|entry| entry.path == "new.txt")
        .expect("the untracked file is listed");
    assert!(untracked.untracked);
    assert_eq!(untracked.orig_path, None);

    let renamed = status
        .entries
        .iter()
        .find(|entry| entry.path == "moved.txt")
        .expect("the rename is listed");
    assert_eq!(renamed.index, GitFileState::Renamed);
    assert_eq!(renamed.worktree, GitFileState::Unchanged);
    assert_eq!(renamed.orig_path.as_deref(), Some("gone.txt"));
}

// ---------------------------------------------------------------------------------------
// One commit

#[tokio::test]
async fn a_commit_has_its_author_message_and_parents() {
    let t = TestHub::start().await;
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
    let root = commit(repo.path(), "first");
    std::fs::write(repo.path().join("a.txt"), "two\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(
        repo.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "Add the thing",
            "-m",
            "A longer explanation.",
        ],
    );
    let second = git(repo.path(), &["rev-parse", "HEAD"]);

    t.create_agent("watcher", repo.path()).await;
    let detail: GitCommitResponse = t
        .git_get(&format!("/v1/git/commits/{second}"), repo.path())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(detail.sha, second);
    assert_eq!(detail.parents, [root.as_str()]);
    assert_eq!(detail.author, "Test");
    assert_eq!(detail.author_email, "test@example.com");
    assert_eq!(detail.subject, "Add the thing");
    assert!(
        detail.message.contains("Add the thing")
            && detail.message.contains("A longer explanation."),
        "{}",
        detail.message
    );
    assert!(detail.time > chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);

    // A root commit has no parents.
    let first: GitCommitResponse = t
        .git_get(&format!("/v1/git/commits/{root}"), repo.path())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(first.parents.is_empty());

    // A sha that is not one is refused before git sees it; one that names nothing is 404.
    for bad in ["HEAD", "not-a-sha", "ABCDEF"] {
        assert_eq!(
            t.git_get(&format!("/v1/git/commits/{bad}"), repo.path())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(
        t.git_get("/v1/git/commits/deadbeef", repo.path())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

// ---------------------------------------------------------------------------------------
// The whole repository's graph

#[tokio::test]
async fn the_graph_covers_every_ref_and_honours_the_limit() {
    let t = TestHub::start().await;
    let (repo, root) = repository();
    git(repo.path(), &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.path().join("b.txt"), "two\n").unwrap();
    let side = commit(repo.path(), "two");
    git(repo.path(), &["tag", "v1", &root]);
    git(
        repo.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "tag",
            "-a",
            "v2",
            "-m",
            "a release",
            &side,
        ],
    );
    git(repo.path(), &["checkout", "-q", "main"]);

    t.create_agent("watcher", repo.path()).await;
    let graph: GitGraph = t
        .git_get("/v1/git/graph", repo.path())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let named: Vec<(&str, GitRefKind)> = graph
        .refs
        .iter()
        .map(|entry| (entry.name.as_str(), entry.kind))
        .collect();
    assert!(named.contains(&("HEAD", GitRefKind::Head)), "{named:?}");
    assert!(named.contains(&("main", GitRefKind::Local)), "{named:?}");
    assert!(named.contains(&("side", GitRefKind::Local)), "{named:?}");
    assert!(named.contains(&("v1", GitRefKind::Tag)), "{named:?}");
    assert!(named.contains(&("v2", GitRefKind::Tag)), "{named:?}");
    // An annotated tag names the commit it is on, not its tag object.
    let annotated = graph.refs.iter().find(|entry| entry.name == "v2").unwrap();
    assert_eq!(annotated.sha, side);

    // `--all` reaches the commit on `side`, which HEAD cannot.
    let side_commit = graph
        .commits
        .iter()
        .find(|commit| commit.sha == side)
        .expect("the side commit is in the graph");
    let mut named = side_commit.refs.clone();
    named.sort();
    assert_eq!(named, ["side", "v2"]);
    assert!(graph.commits.iter().any(|commit| commit.sha == root));
    assert!(!graph.truncated);

    let limited: GitGraph = t
        .get(&format!(
            "/v1/git/graph?path={}&limit=1",
            query_escape(&repo.path().to_string_lossy())
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(limited.commits.len(), 1);
    assert!(limited.truncated);
}

// ---------------------------------------------------------------------------------------
// Which directories may be read

#[tokio::test]
async fn only_project_directories_the_hub_knows_are_read() {
    let t = TestHub::start().await;
    let (repo, _) = repository();
    t.create_agent("watcher", repo.path()).await;

    // The agent's working directory is a project.
    assert_eq!(
        t.git_get("/v1/git/branches", repo.path())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // A path the hub does not know, even an existing repository, is refused.
    let (stranger, _) = repository();
    assert_eq!(
        t.git_get("/v1/git/branches", stranger.path())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    // `..` may not lead out of a project.
    let escape = format!("{}/../stranger", repo.path().display());
    assert_eq!(
        t.get(&format!("/v1/git/status?path={}", query_escape(&escape)))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    // Nor may a relative path, or an absolute one that is not there.
    assert_eq!(
        t.get("/v1/git/graph?path=repo")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let missing = repo.path().join("does-not-exist");
    assert_eq!(
        t.git_get("/v1/git/graph", &missing)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        t.get("/v1/git/branches").send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );

    // A symlink inside the project that leads outside it is refused too.
    let link = repo.path().join("outside");
    let linked = symlink_dir(stranger.path(), &link);
    if linked {
        assert_eq!(
            t.git_get("/v1/git/branches", &link)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        std::fs::remove_file(&link)
            .or_else(|_| std::fs::remove_dir(&link))
            .ok();
    }
    #[cfg(unix)]
    assert!(linked, "a symlink could not be created");

    // A directory the hub knows but that is not a repository is a bad request.
    let plain = tempfile::tempdir().unwrap();
    t.create_agent("plain", plain.path()).await;
    assert_eq!(
        t.git_get("/v1/git/status", plain.path())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    // Reading is all the read-only token may do here.
    let status = t
        .client
        .get(t.url(&format!(
            "/v1/git/status?path={}",
            query_escape(&repo.path().to_string_lossy())
        )))
        .bearer_auth(&t.hub.read_token)
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
}

/// Creates a directory symlink at `link` pointing at `target`; `false` when the platform
/// does not let this test make one.
fn symlink_dir(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

// ---------------------------------------------------------------------------------------
// A run's repository

#[tokio::test]
async fn a_runs_repository_is_a_project() {
    // The run's lead is a real plugin agent, so the run gets a checkout; its CLI is not
    // there, so the kickoff it is handed never starts a process.
    let t = TestHub::start_with(|config| {
        config.plugins.insert(
            "claude".into(),
            crossagents_plugins::PluginConfig {
                native_command: vec!["crossagents-test-no-such-cli".into()],
                ..Default::default()
            },
        );
    })
    .await;
    let (repo, root) = repository();
    t.create_agent("planner", repo.path()).await;

    let created = t
        .post("/v1/runs")
        .json(&json!({ "request": "look at the history", "planner": "planner" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        created.status(),
        StatusCode::CREATED,
        "{}",
        created.text().await.unwrap()
    );
    let run: Run = created.json().await.unwrap();
    assert_eq!(run.repo, dunce::canonicalize(repo.path()).unwrap());

    // The run's repository is one of the directories the view may read.
    let roots = t.hub.orchestrator.git_roots().await.unwrap();
    assert!(
        roots.contains(&dunce::canonicalize(&run.repo).unwrap()),
        "{roots:?}"
    );

    // The run's repository is read, and the run's branches are part of it.
    let branches: GitBranchesResponse = t
        .git_get("/v1/git/branches", &run.repo)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branches.head_sha.as_deref(), Some(root.as_str()));
    assert!(
        branches
            .branches
            .iter()
            .any(|branch| branch.name == run.branch),
        "{:?}",
        branches.branches
    );
}

// ---------------------------------------------------------------------------------------
// Read-only

#[tokio::test]
async fn the_view_never_fetches_or_moves_a_branch() {
    let t = TestHub::start().await;
    let (origin, other) = origin_with_a_clone();
    let repo = tempfile::tempdir().unwrap();
    git(
        repo.path(),
        &["clone", "-q", origin.path().to_str().unwrap(), "."],
    );
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let tracked = git(repo.path(), &["rev-parse", "origin/main"]);
    t.create_agent("watcher", repo.path()).await;

    // origin moves on, so a fetch during any request would move origin/main.
    std::fs::write(other.path().join("b.txt"), "two\n").unwrap();
    commit(other.path(), "two");
    git(other.path(), &["push", "-q", "origin", "main"]);

    for route in ["/v1/git/branches", "/v1/git/status", "/v1/git/graph"] {
        assert_eq!(
            t.git_get(route, repo.path()).send().await.unwrap().status(),
            StatusCode::OK,
            "{route}"
        );
    }
    assert_eq!(
        t.git_get(&format!("/v1/git/commits/{head}"), repo.path())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(repo.path(), &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
    assert_eq!(git(repo.path(), &["rev-parse", "origin/main"]), tracked);
    assert!(
        git(repo.path(), &["status", "--porcelain"]).is_empty(),
        "the working copy was changed"
    );
}
