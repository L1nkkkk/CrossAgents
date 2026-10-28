//! Which project a directory belongs to (docs/tasks/LESSONS.md §4.2). A project has a stable
//! id; its remotes recognise it on any node, and each node's checkout is bound to it by path.
//! A path never moves to another project by itself: a remote seen there that the project does
//! not have marks it for the person to confirm.

use std::path::{Path, PathBuf};

use chrono::Utc;
use crossagents_core::{CoreError, Project, ProjectPath};

use crate::engine::Orchestrator;

/// The node this hub's own checkouts are on, until there are others (docs/MULTI-NODE.md).
pub const LOCAL_NODE: &str = "local";

/// `host/owner/repo` for a remote URL, so the SSH and HTTPS forms of one repository are equal:
/// scheme, user, port, `.git` and trailing slashes dropped, the host lower-cased. `None` for a
/// remote that is a path on this machine, which says nothing on another one.
pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.starts_with("file:") || url.starts_with('/') || url.starts_with('.') {
        return None;
    }
    let (host, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        // scp-like `git@host:owner/repo`; a Windows path (`C:\…`, `C:/…`) is not one.
        None => {
            let (host, path) = url.split_once(':')?;
            if host.len() == 1 || path.starts_with('\\') || path.starts_with("//") {
                return None;
            }
            (host, path)
        }
    };
    let host = host.rsplit('@').next()?;
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{path}", host.to_lowercase()))
}

/// What to store after looking `root` (with its `remote`) up among `projects`, and whether it
/// changed. Kept free of I/O for the tests.
pub fn resolve(projects: &[Project], root: &Path, remote: Option<&str>) -> (Project, bool) {
    let bound = projects.iter().find(|p| {
        p.paths
            .iter()
            .any(|at| at.node == LOCAL_NODE && same_path(&at.path, root))
    });
    if let Some(project) = bound {
        let mut project = project.clone();
        // The directory may hold another repository now: the person says which project.
        let stranger = remote.filter(|r| !project.remotes.iter().any(|own| own == r));
        let changed = match stranger {
            Some(r) if project.unconfirmed_remote.as_deref() != Some(r) => {
                project.unconfirmed_remote = Some(r.to_owned());
                true
            }
            _ => false,
        };
        return (project, changed);
    }
    let here = ProjectPath {
        node: LOCAL_NODE.into(),
        path: root.to_path_buf(),
    };
    let mut fresh = Project {
        id: uuid::Uuid::new_v4().to_string(),
        name: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        remotes: Vec::new(),
        paths: vec![here.clone()],
        unconfirmed_remote: None,
        created_at: Utc::now(),
    };
    let Some(remote) = remote else {
        return (fresh, true);
    };
    let known: Vec<&Project> = projects
        .iter()
        .filter(|p| p.remotes.iter().any(|r| r == remote))
        .collect();
    match known.as_slice() {
        [] => fresh.remotes.push(remote.to_owned()),
        // One project has it and is not waiting on the person: another checkout of it.
        [one] if one.unconfirmed_remote.is_none() => {
            let mut project = (*one).clone();
            project.paths.push(here);
            return (project, true);
        }
        // Several, or one whose identity is in question: the person binds it.
        _ => fresh.unconfirmed_remote = Some(remote.to_owned()),
    }
    (fresh, true)
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// The main checkout `dir` lies in: a worktree's repository, else the directory holding
/// `.git`. `None` outside any repository.
pub fn main_repo(dir: &Path) -> Option<PathBuf> {
    let mut at = Some(dir);
    while let Some(d) = at {
        let git = d.join(".git");
        if git.is_dir() {
            return dunce::canonicalize(d).ok();
        }
        if git.is_file() {
            return crate::workspace::common_repo(d);
        }
        at = d.parent();
    }
    None
}

impl Orchestrator {
    /// The project `dir` belongs to, made or bound on first sight (LESSONS §4.2).
    pub async fn project_for(&self, dir: &Path) -> Result<Project, CoreError> {
        let (root, remote) = match main_repo(dir) {
            Some(root) => {
                let remote = self.workspaces.remote(&root).await;
                (root, remote.as_deref().and_then(normalize_remote))
            }
            None => (
                dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()),
                None,
            ),
        };
        // One at a time, so two first sights of a repository make one project.
        let _held = self.projects_lock.lock().await;
        let projects = self.store.list_projects().await?;
        let (project, changed) = resolve(&projects, &root, remote.as_deref());
        if changed {
            self.store.put_project(&project).await?;
        }
        Ok(project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_repository_has_one_remote_however_it_is_written() {
        let forms = [
            "git@github.com:L1nkkkk/CrossAgents.git",
            "https://github.com/L1nkkkk/CrossAgents",
            "https://user@GitHub.com/L1nkkkk/CrossAgents.git/",
            "ssh://git@github.com:22/L1nkkkk/CrossAgents.git",
        ];
        for form in forms {
            assert_eq!(
                normalize_remote(form).as_deref(),
                Some("github.com/L1nkkkk/CrossAgents"),
                "{form}"
            );
        }
        // A fork is another repository.
        assert_ne!(
            normalize_remote("git@github.com:someone/CrossAgents.git"),
            normalize_remote(forms[0])
        );
        for local in [
            "/srv/git/repo.git",
            "C:\\repos\\x",
            "C:/repos/x",
            "file:///x",
            "../x",
            "",
        ] {
            assert_eq!(normalize_remote(local), None, "{local}");
        }
    }

    fn project(remotes: &[&str], path: &str) -> Project {
        let (p, _) = resolve(&[], Path::new(path), None);
        Project {
            remotes: remotes.iter().map(|r| r.to_string()).collect(),
            ..p
        }
    }

    #[test]
    fn checkouts_of_one_remote_share_a_project_and_forks_do_not() {
        let main = project(&["github.com/o/r"], "/a/r");
        let projects = vec![main.clone()];
        // Another node, same remote: bound to it.
        let (other, changed) = resolve(&projects, Path::new("/b/r"), Some("github.com/o/r"));
        assert!(changed);
        assert_eq!(other.id, main.id);
        assert_eq!(other.paths.len(), 2);
        // A fork: its own project.
        let (fork, _) = resolve(&projects, Path::new("/b/fork"), Some("github.com/me/r"));
        assert_ne!(fork.id, main.id);
        assert_eq!(fork.remotes, vec!["github.com/me/r".to_owned()]);
        // Seen again: nothing to store.
        let (again, changed) = resolve(&projects, Path::new("/a/r"), Some("github.com/o/r"));
        assert_eq!(again, main);
        assert!(!changed);
        // No remote: a project of its own path.
        let (plain, _) = resolve(&projects, Path::new("/c/notes"), None);
        assert!(plain.remotes.is_empty());
        assert_ne!(plain.id, main.id);
    }

    #[test]
    fn a_strange_remote_at_a_bound_path_waits_for_the_person() {
        let main = project(&["github.com/o/r"], "/a/r");
        // The directory now holds a fork: still the same project, marked, nothing rebound.
        let (marked, changed) = resolve(
            std::slice::from_ref(&main),
            Path::new("/a/r"),
            Some("github.com/me/r"),
        );
        assert!(changed);
        assert_eq!(marked.id, main.id);
        assert_eq!(marked.remotes, main.remotes);
        assert_eq!(
            marked.unconfirmed_remote.as_deref(),
            Some("github.com/me/r")
        );
        // Meanwhile a new checkout of the original remote is not bound to it automatically.
        let (other, _) = resolve(
            std::slice::from_ref(&marked),
            Path::new("/b/r"),
            Some("github.com/o/r"),
        );
        assert_ne!(other.id, main.id);
        assert_eq!(other.unconfirmed_remote.as_deref(), Some("github.com/o/r"));
        // A project made without a remote that gains one is asked about too.
        let plain = project(&[], "/c/x");
        let (asked, _) = resolve(
            std::slice::from_ref(&plain),
            Path::new("/c/x"),
            Some("github.com/o/x"),
        );
        assert_eq!(asked.id, plain.id);
        assert!(asked.remotes.is_empty());
        assert_eq!(asked.unconfirmed_remote.as_deref(), Some("github.com/o/x"));
    }
}
