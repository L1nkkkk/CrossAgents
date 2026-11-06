//! `crossagents lessons`: a project's troubleshooting records (docs/tasks/LESSONS.md L2).
//!
//! A record is reference for later agents, not a rule: what is printed here says what was
//! tried and how it went, so check its conditions before going by it.

use std::path::Path;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use crossagents_api_types::{
    Lesson, LessonOutcome, LessonStatus, ListLessonsResponse, ListProjectsResponse, ProjectInfo,
};

use super::Context;
use crate::client::Client;
use crate::output::print_json;

#[derive(Debug, Subcommand)]
pub enum LessonsCommand {
    /// List the project's records, newest first.
    Ls(ListArgs),
    /// One record in full: conditions, resolution, evidence and sources.
    Show(ShowArgs),
    /// Search the records by keywords or an error message.
    Search(SearchArgs),
    /// The project's records as Markdown, on stdout.
    Export(ProjectArg),
}

#[derive(Debug, Args)]
pub struct ProjectArg {
    /// Project id or name; by default the project whose checkout holds the current directory.
    #[arg(long)]
    pub project: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub project: ProjectArg,
    /// Only records with this status: active, review or superseded.
    #[arg(long)]
    pub status: Option<String>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// The error text or keywords.
    pub query: Vec<String>,
    #[command(flatten)]
    pub project: ProjectArg,
    #[arg(long)]
    pub status: Option<String>,
}

pub async fn run(ctx: &Context, cmd: LessonsCommand) -> Result<()> {
    let client = ctx.client()?;
    match cmd {
        LessonsCommand::Ls(args) => {
            let project = resolve_project(&client, args.project.project.as_deref()).await?;
            let path = list_path(&project.id, None, args.status.as_deref())?;
            print_list(ctx, client.get(&path).await?)
        }
        LessonsCommand::Search(args) => {
            let query = args.query.join(" ");
            if query.trim().is_empty() {
                bail!("give the error text or keywords to search for");
            }
            let project = resolve_project(&client, args.project.project.as_deref()).await?;
            let path = list_path(&project.id, Some(&query), args.status.as_deref())?;
            print_list(ctx, client.get(&path).await?)
        }
        LessonsCommand::Show(args) => {
            let lesson: Lesson = client
                .get(&format!("/v1/lessons/{}", encode(args.id.trim())))
                .await?;
            if ctx.json {
                return print_json(&lesson);
            }
            print!("{}", render_lesson(&lesson));
            Ok(())
        }
        LessonsCommand::Export(args) => {
            let project = resolve_project(&client, args.project.as_deref()).await?;
            let markdown = client
                .get_text(&format!("/v1/projects/{}/lessons/export", encode(&project.id)))
                .await?;
            if ctx.json {
                return print_json(&serde_json::json!({ "markdown": markdown }));
            }
            if markdown.trim().is_empty() {
                println!("project {} has no troubleshooting record yet", project.name);
                return Ok(());
            }
            print!("{markdown}");
            Ok(())
        }
    }
}

fn list_path(project: &str, query: Option<&str>, status: Option<&str>) -> Result<String> {
    let mut params = Vec::new();
    if let Some(q) = query {
        params.push(format!("q={}", encode(q)));
    }
    if let Some(status) = status {
        let status = status.trim().to_ascii_lowercase();
        if !matches!(status.as_str(), "active" | "review" | "superseded") {
            bail!("--status is active, review or superseded");
        }
        params.push(format!("status={status}"));
    }
    let mut path = format!("/v1/projects/{}/lessons", encode(project));
    if !params.is_empty() {
        path.push('?');
        path.push_str(&params.join("&"));
    }
    Ok(path)
}

/// The project named, or the one with a checkout holding the current directory, or the only
/// one there is.
async fn resolve_project(client: &Client, wanted: Option<&str>) -> Result<ProjectInfo> {
    let list: ListProjectsResponse = client.get("/v1/projects").await?;
    let cwd = std::env::current_dir().ok();
    pick_project(list.projects, wanted, cwd.as_deref())
}

fn pick_project(
    projects: Vec<ProjectInfo>,
    wanted: Option<&str>,
    cwd: Option<&Path>,
) -> Result<ProjectInfo> {
    if let Some(wanted) = wanted.map(str::trim).filter(|w| !w.is_empty()) {
        if let Some(p) = projects.iter().find(|p| p.id == wanted) {
            return Ok(p.clone());
        }
        let named: Vec<_> = projects.iter().filter(|p| p.name == wanted).collect();
        return match named.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("no project {wanted}; `crossagents lessons ls --project <id>` takes an id or name"),
            _ => bail!("several projects are called {wanted}; give its id"),
        };
    }
    if let Some(cwd) = cwd {
        let cwd = normalise(&cwd.to_string_lossy());
        let mut best: Option<(usize, &ProjectInfo)> = None;
        for p in &projects {
            for path in &p.paths {
                let root = normalise(&path.path);
                if !root.is_empty()
                    && (cwd == root || cwd.starts_with(&format!("{root}/")))
                    && best.is_none_or(|(len, _)| root.len() > len)
                {
                    best = Some((root.len(), p));
                }
            }
        }
        if let Some((_, p)) = best {
            return Ok(p.clone());
        }
    }
    match projects.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("the hub knows no project yet"),
        _ => {
            let names: Vec<String> = projects
                .iter()
                .map(|p| format!("{} ({})", p.name, p.id))
                .collect();
            bail!(
                "the current directory is in no known project; pick one with --project: {}",
                names.join(", ")
            )
        }
    }
}

/// Compares paths the way the file system does on Windows: separators and case do not matter.
fn normalise(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = p.trim_end_matches('/');
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p.to_owned()
    }
}

fn print_list(ctx: &Context, list: ListLessonsResponse) -> Result<()> {
    if ctx.json {
        return print_json(&list);
    }
    if list.lessons.is_empty() {
        println!("no troubleshooting record found");
        return Ok(());
    }
    for l in &list.lessons {
        println!(
            "{}  {:<10} {:<10} {}",
            short(&l.id),
            status_word(l.status),
            outcome_word(l.outcome),
            l.title
        );
    }
    println!("\nA record is reference, not a rule: check its conditions (`crossagents lessons show <id>`).");
    Ok(())
}

fn render_lesson(l: &Lesson) -> String {
    let mut out = format!("{}\n", l.title);
    out.push_str(&format!("id:        {}\n", l.id));
    out.push_str(&format!(
        "status:    {}{}\n",
        status_word(l.status),
        l.superseded_by
            .as_deref()
            .map(|s| format!(" (by {s})"))
            .unwrap_or_default()
    ));
    out.push_str(&format!("outcome:   {}\n", outcome_word(l.outcome)));
    if !l.keywords.is_empty() {
        out.push_str(&format!("keywords:  {}\n", l.keywords.join(", ")));
    }
    out.push_str(&format!("updated:   {}\n", l.updated_at.format("%Y-%m-%d %H:%M")));
    out.push_str(&format!("\nSymptom\n{}\n", l.symptom.trim()));
    if !l.conditions.trim().is_empty() {
        out.push_str(&format!("\nConditions\n{}\n", l.conditions.trim()));
    }
    out.push_str(&format!("\nResolution\n{}\n", l.resolution.trim()));
    if !l.evidence.is_empty() {
        out.push_str(&format!("\nTried {} time(s)\n", l.evidence.len()));
        for e in l.evidence.iter().rev() {
            let note = if e.note.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", e.note.trim())
            };
            out.push_str(&format!(
                "  {}  {:<10} {} on {}{}\n",
                e.at.format("%Y-%m-%d %H:%M"),
                outcome_word(e.outcome),
                e.agent,
                e.node,
                note
            ));
        }
    }
    if !l.sources.is_empty() {
        out.push_str("\nFrom\n");
        for s in &l.sources {
            out.push_str(&format!("  run {} {}\n", s.run_id, s.key));
        }
    }
    out
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn status_word(s: LessonStatus) -> &'static str {
    match s {
        LessonStatus::Active => "active",
        LessonStatus::Review => "review",
        LessonStatus::Superseded => "superseded",
    }
}

fn outcome_word(o: LessonOutcome) -> &'static str {
    match o {
        LessonOutcome::Resolved => "resolved",
        LessonOutcome::Workaround => "workaround",
        LessonOutcome::Unresolved => "unresolved",
    }
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_api_types::ProjectPathInfo;

    fn project(id: &str, name: &str, path: &str) -> ProjectInfo {
        ProjectInfo {
            id: id.into(),
            name: name.into(),
            remotes: vec![],
            paths: vec![ProjectPathInfo {
                node: "local".into(),
                path: path.into(),
            }],
        }
    }

    #[test]
    fn the_project_is_named_or_found_from_the_directory() {
        let ps = vec![project("p1", "alpha", "/src/alpha"), project("p2", "beta", "/src/beta")];
        assert_eq!(pick_project(ps.clone(), Some("p2"), None).unwrap().id, "p2");
        assert_eq!(pick_project(ps.clone(), Some("alpha"), None).unwrap().id, "p1");
        assert!(pick_project(ps.clone(), Some("nope"), None).is_err());
        let cwd = Path::new("/src/beta/crates/x");
        assert_eq!(pick_project(ps.clone(), None, Some(cwd)).unwrap().id, "p2");
        assert!(pick_project(ps.clone(), None, Some(Path::new("/elsewhere"))).is_err());
        let one = vec![project("p1", "alpha", "/src/alpha")];
        assert_eq!(pick_project(one, None, Some(Path::new("/elsewhere"))).unwrap().id, "p1");
    }

    #[test]
    fn list_paths_carry_the_query_and_status() {
        assert_eq!(list_path("p1", None, None).unwrap(), "/v1/projects/p1/lessons");
        assert_eq!(
            list_path("p1", Some("E0425 link"), Some("Active")).unwrap(),
            "/v1/projects/p1/lessons?q=E0425%20link&status=active"
        );
        assert!(list_path("p1", None, Some("gone")).is_err());
    }
}
