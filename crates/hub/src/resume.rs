//! The command that carries a session on in its own CLI, in a terminal (docs/TODO.md §十五):
//! `GET /v1/agents/{key}/resume-command`. The hub keeps each session's native id and resumes
//! by it; a person can do the same with the vendor's CLI, given the directory, the account's
//! config directory and the proxy the hub would use. Nothing here runs anything.
//!
//! What goes into the command is chosen, not copied: of the session's environment only the
//! config directories and proxies, so no token is ever printed (an account's login stays in
//! its directory, which the command only names).

use std::path::{Path, PathBuf};

use crossagents_api_types::{ResumeCommandResponse, ResumeEnv};
use crossagents_core::DriverKind;

/// Where an account keeps its login: the only account variables a command carries.
const CONFIG_DIRS: [&str; 3] = ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "CODEBUDDY_CONFIG_DIR"];
/// Proxies, as the hub hands them to the CLI.
const PROXIES: [&str; 5] = [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    PowerShell,
    Sh,
}

impl Shell {
    /// PowerShell on Windows (what its terminal opens), `sh` elsewhere.
    pub fn here() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::Sh
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::Sh => "sh",
        }
    }

    fn quote(self, s: &str) -> String {
        match self {
            Self::PowerShell => format!("'{}'", s.replace('\'', "''")),
            Self::Sh => format!("'{}'", s.replace('\'', r"'\''")),
        }
    }
}

/// What the hub knows about the session, gathered by `Runtime::resume_command`.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub name: String,
    /// Not a CLI the hub runs (`external`): nothing to resume it with.
    pub external: bool,
    /// `None`: its plugin has no driver here.
    pub kind: Option<DriverKind>,
    /// The driver's `resume` capability.
    pub resumes: bool,
    pub session_id: Option<String>,
    /// `config.context_lost`: its next session will be a new one.
    pub context_lost: Option<String>,
    pub cwd: PathBuf,
    pub cwd_exists: bool,
    /// Archived away from its workspace: a lead copied for a run, whose checkout went.
    pub moved_away: bool,
    /// How the hub launches the CLI (`claude.exe`, or `node cli.js`); `None`: not found.
    pub program: Option<Vec<String>>,
    /// The session's environment and the proxy, unfiltered: [`build`] keeps what it may print.
    pub env: Vec<(String, String)>,
    /// The hub has the session open now.
    pub live: bool,
    /// Where the CLI keeps the session's file, when it was found.
    pub session_file: Option<PathBuf>,
}

pub fn build(f: &Facts, shell: Shell) -> ResumeCommandResponse {
    let mut out = ResumeCommandResponse {
        shell: shell.name().to_owned(),
        cwd: f.cwd.display().to_string(),
        session_id: f.session_id.clone(),
        ..ResumeCommandResponse::default()
    };
    let (env, stripped) = printable_env(&f.env);
    out.env = env;
    match command(f, shell, &out.env) {
        Ok(command) => out.command = Some(command),
        Err(why) => {
            out.unavailable = Some(why);
            return out;
        }
    }
    if stripped {
        out.notes
            .push("代理地址里的用户名和密码没有写进命令，需要的话自己补上".into());
    }
    if f.live {
        out.notes.push(format!(
            "{} 现在在 hub 里开着：两边同时用同一个会话会互相覆盖，先在 GUI 里停掉它，再在终端里接着聊",
            f.name
        ));
    }
    if let Some(file) = &f.session_file
        && is_claude_flavor(f.kind)
        && file
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|dir| !is_project_dir_of(&dir.to_string_lossy(), &f.cwd))
    {
        out.notes.push(format!(
            "会话文件在 {}：CLI 按当前目录列出会话，在这个目录直接打开恢复列表可能看不到它，按 id 恢复不受影响",
            file.display()
        ));
    }
    if shell == Shell::PowerShell && !out.env.is_empty() {
        out.notes
            .push("命令里用 $env: 设的变量会留在这个 PowerShell 窗口里".into());
    }
    out
}

/// The command, or why there is none. The checks go from what can never work to what the
/// person can fix.
fn command(f: &Facts, shell: Shell, env: &[ResumeEnv]) -> Result<String, String> {
    let name = &f.name;
    if f.external {
        return Err(format!(
            "{name} 是外部接入的会话，CrossAgents 不知道它的 CLI 怎么恢复"
        ));
    }
    let Some(kind) = f.kind else {
        return Err(format!("{name} 的插件在这台机器上没有可用的驱动"));
    };
    if !f.resumes {
        return Err(format!(
            "{name} 的驱动（{kind}）不能恢复会话：进程停了，上下文就没了"
        ));
    }
    let resume: Vec<String> = match kind {
        DriverKind::ClaudeNative | DriverKind::CodebuddyNative => vec!["--resume".into()],
        DriverKind::CodexNative => vec!["resume".into()],
        DriverKind::CursorNative => vec!["--resume".into()],
        DriverKind::WithNative | DriverKind::AcpGeneric => {
            return Err(format!(
                "{kind} 驱动的 CLI 没有在终端里按 session id 接着聊的命令"
            ));
        }
    };
    let Some(id) = &f.session_id else {
        return Err(format!("{name} 还没开过会话，没有可以恢复的 session id"));
    };
    if let Some(why) = &f.context_lost {
        return Err(format!("原来的上下文已经带不回来：{why}"));
    }
    if !f.cwd_exists {
        let mut why = format!("目录 {} 不在了", f.cwd.display());
        if f.moved_away {
            why.push_str("：复制的主导在 run 结束后检出被删掉了。先在这里点「恢复对话」，hub 会重建检出，之后再复制命令");
        }
        return Err(why);
    }
    let Some(program) = f.program.as_ref().filter(|p| !p.is_empty()) else {
        return Err(format!("这台机器上找不到 {name} 用的 CLI"));
    };
    let mut argv = program.clone();
    // `codex app-server` is how the hub talks to Codex; a person talks to `codex` itself.
    if kind == DriverKind::CodexNative && argv.last().is_some_and(|a| a == "app-server") {
        argv.pop();
    }
    argv.extend(resume);
    argv.push(id.clone());
    let args = argv
        .iter()
        .map(|a| quote_arg(shell, a))
        .collect::<Vec<_>>()
        .join(" ");
    let cwd = f.cwd.display().to_string();
    Ok(match shell {
        Shell::PowerShell => {
            let mut parts = vec![format!("Set-Location -LiteralPath {}", shell.quote(&cwd))];
            parts.extend(
                env.iter()
                    .map(|e| format!("$env:{} = {}", e.name, shell.quote(&e.value))),
            );
            parts.push(format!("& {args}"));
            parts.join("; ")
        }
        Shell::Sh => {
            let vars: String = env
                .iter()
                .map(|e| format!("{}={} ", e.name, shell.quote(&e.value)))
                .collect();
            format!("cd {} && {vars}{args}", shell.quote(&cwd))
        }
    })
}

/// Plain flags stay as they are (`--resume`), so the command reads like one typed by hand.
fn quote_arg(shell: Shell, arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !arg.starts_with('.');
    if plain && arg.starts_with('-') {
        arg.to_owned()
    } else {
        shell.quote(arg)
    }
}

/// The config directories and proxies of `env`, first value of each name, and whether a
/// proxy's user and password were taken out.
fn printable_env(env: &[(String, String)]) -> (Vec<ResumeEnv>, bool) {
    let mut out: Vec<ResumeEnv> = Vec::new();
    let mut stripped = false;
    for (name, value) in env {
        let config = CONFIG_DIRS.contains(&name.as_str());
        let proxy = PROXIES.contains(&name.as_str());
        if !(config || proxy) || value.is_empty() || out.iter().any(|e| &e.name == name) {
            continue;
        }
        let value = if proxy {
            let (clean, had) = without_credentials(value);
            stripped |= had;
            clean
        } else {
            value.clone()
        };
        out.push(ResumeEnv {
            name: name.clone(),
            value,
        });
    }
    (out, stripped)
}

/// `http://user:pass@host:8080` → `http://host:8080`.
fn without_credentials(url: &str) -> (String, bool) {
    let (scheme, rest) = url.split_once("://").map_or(("", url), |(s, r)| (s, r));
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        return (url.to_owned(), false);
    };
    let clean = &rest[at + 1..];
    let clean = if scheme.is_empty() {
        clean.to_owned()
    } else {
        format!("{scheme}://{clean}")
    };
    (clean, true)
}

fn is_claude_flavor(kind: Option<DriverKind>) -> bool {
    matches!(
        kind,
        Some(DriverKind::ClaudeNative | DriverKind::CodebuddyNative)
    )
}

/// Whether `folder` (a name under `<config>/projects/`) is where the CLI keeps `cwd`'s
/// sessions. The CLIs spell it differently — Claude Code `C--repo--crossagents-…`, CodeBuddy
/// `c-Users-me-.crossagents-…` — so only letters and digits are compared, in any case.
pub fn is_project_dir_of(folder: &str, cwd: &Path) -> bool {
    let key = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    key(folder) == key(&cwd.display().to_string())
}

/// `<config>/projects/*/<id>.jsonl`, for a Claude-flavoured CLI whose config directory is
/// `config`.
pub fn find_session_file(config: &Path, id: &str) -> Option<PathBuf> {
    let file = format!("{id}.jsonl");
    std::fs::read_dir(config.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join(&file))
        .find(|p| p.is_file())
}

/// The config directory a Claude-flavoured CLI uses with `env`: the account's, else the one
/// the hub was started with, else the default under the user's home.
pub fn claude_config_dir(kind: DriverKind, env: &[(String, String)]) -> Option<PathBuf> {
    let (var, default) = match kind {
        DriverKind::ClaudeNative => ("CLAUDE_CONFIG_DIR", ".claude"),
        DriverKind::CodebuddyNative => ("CODEBUDDY_CONFIG_DIR", ".codebuddy"),
        _ => return None,
    };
    env.iter()
        .find(|(k, _)| k == var)
        .map(|(_, v)| PathBuf::from(v))
        .or_else(|| std::env::var_os(var).map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|h| h.join(default)))
}

/// The proxy the hub hands a CLI of `kind`: its own when it was started with one (children
/// inherit it), else for Claude-flavoured CLIs the system proxy it passes on
/// (`crossagents_drivers::process::system_proxy_env`).
pub fn proxy_env(kind: Option<DriverKind>) -> Vec<(String, String)> {
    let own: Vec<(String, String)> = PROXIES
        .iter()
        .filter_map(|k| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| ((*k).to_owned(), v))
        })
        .collect();
    if !own.is_empty() || !is_claude_flavor(kind) {
        return own;
    }
    crossagents_drivers::process::system_proxy_env()
        .into_iter()
        .filter(|(k, _)| PROXIES.contains(&k.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude(dir: &Path) -> Facts {
        Facts {
            name: "Rex".into(),
            kind: Some(DriverKind::ClaudeNative),
            resumes: true,
            session_id: Some("0b5c-11".into()),
            cwd: dir.to_path_buf(),
            cwd_exists: true,
            program: Some(vec![r"C:\Users\me\.local\bin\claude.exe".into()]),
            env: vec![
                (
                    "CLAUDE_CONFIG_DIR".into(),
                    r"C:\Users\me\.claude-work".into(),
                ),
                ("CROSSAGENTS_TOKEN".into(), "secret-token".into()),
                ("ANTHROPIC_API_KEY".into(), "sk-ant-secret".into()),
                ("CARGO_TARGET_DIR".into(), r"C:\repo\target".into()),
                ("HTTPS_PROXY".into(), "http://127.0.0.1:7890".into()),
            ],
            ..Facts::default()
        }
    }

    #[test]
    fn powershell_goes_to_the_directory_sets_the_account_and_resumes_by_id() {
        let f = claude(Path::new(r"C:\work\it's here"));
        let r = build(&f, Shell::PowerShell);
        assert_eq!(r.unavailable, None);
        assert_eq!(
            r.command.as_deref(),
            Some(
                r"Set-Location -LiteralPath 'C:\work\it''s here'; $env:CLAUDE_CONFIG_DIR = 'C:\Users\me\.claude-work'; $env:HTTPS_PROXY = 'http://127.0.0.1:7890'; & 'C:\Users\me\.local\bin\claude.exe' --resume '0b5c-11'"
            )
        );
        assert_eq!(r.shell, "powershell");
        let names: Vec<&str> = r.env.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["CLAUDE_CONFIG_DIR", "HTTPS_PROXY"]);
    }

    #[test]
    fn no_secret_reaches_the_command() {
        let r = build(&claude(Path::new("/w")), Shell::Sh);
        let command = r.command.unwrap();
        for secret in ["secret-token", "sk-ant-secret", "CARGO_TARGET_DIR"] {
            assert!(!command.contains(secret), "{command}");
        }
        assert_eq!(
            command,
            "cd '/w' && CLAUDE_CONFIG_DIR='C:\\Users\\me\\.claude-work' HTTPS_PROXY='http://127.0.0.1:7890' 'C:\\Users\\me\\.local\\bin\\claude.exe' --resume '0b5c-11'"
        );
    }

    #[test]
    fn a_proxy_loses_its_password() {
        let mut f = claude(Path::new("/w"));
        f.env = vec![(
            "HTTPS_PROXY".into(),
            "http://me:hunter2@proxy.corp:8080/".into(),
        )];
        let r = build(&f, Shell::Sh);
        assert!(!r.command.as_deref().unwrap().contains("hunter2"));
        assert_eq!(r.env[0].value, "http://proxy.corp:8080/");
        assert!(
            r.notes.iter().any(|n| n.contains("用户名和密码")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn codex_is_resumed_with_its_subcommand_not_the_app_server() {
        let mut f = claude(Path::new("/w"));
        f.kind = Some(DriverKind::CodexNative);
        f.program = Some(vec!["codex".into(), "app-server".into()]);
        f.env = vec![("CODEX_HOME".into(), "/h/.codex-2".into())];
        let r = build(&f, Shell::Sh);
        assert_eq!(
            r.command.as_deref(),
            Some("cd '/w' && CODEX_HOME='/h/.codex-2' 'codex' 'resume' '0b5c-11'")
        );
    }

    #[test]
    fn cursor_is_resumed_with_its_chat_id() {
        let mut f = claude(Path::new("/w"));
        f.kind = Some(DriverKind::CursorNative);
        f.program = Some(vec!["agent".into()]);
        f.env.clear();
        let r = build(&f, Shell::Sh);
        assert_eq!(
            r.command.as_deref(),
            Some("cd '/w' && 'agent' --resume '0b5c-11'")
        );
    }

    #[test]
    fn each_reason_it_cannot_be_done_is_said() {
        let base = claude(Path::new("/w"));
        let why = |change: &dyn Fn(&mut Facts)| {
            let mut f = base.clone();
            change(&mut f);
            let r = build(&f, Shell::PowerShell);
            assert!(r.command.is_none(), "{r:?}");
            r.unavailable.unwrap()
        };
        assert!(why(&|f| f.session_id = None).contains("还没开过会话"));
        assert!(why(&|f| f.resumes = false).contains("不能恢复会话"));
        assert!(why(&|f| f.kind = Some(DriverKind::WithNative)).contains("没有在终端里"));
        assert!(why(&|f| f.external = true).contains("外部接入"));
        assert!(
            why(&|f| f.context_lost = Some("run「x」删除了".into())).contains("run「x」删除了")
        );
        assert!(why(&|f| f.cwd_exists = false).contains("不在了"));
        let moved = why(&|f| {
            f.cwd_exists = false;
            f.moved_away = true;
        });
        assert!(moved.contains("恢复对话"), "{moved}");
        assert!(why(&|f| f.program = None).contains("找不到"));
    }

    #[test]
    fn says_when_the_session_file_is_kept_for_another_directory() {
        let mut f = claude(Path::new(r"C:\repo"));
        f.session_file = Some(PathBuf::from(
            r"C:\Users\me\.claude\projects\C--repo--crossagents-worktrees-ab12-integration\0b5c-11.jsonl",
        ));
        f.live = true;
        let r = build(&f, Shell::PowerShell);
        assert!(
            r.notes.iter().any(|n| n.contains("会话文件在")),
            "{:?}",
            r.notes
        );
        assert!(
            r.notes.iter().any(|n| n.contains("停掉它")),
            "{:?}",
            r.notes
        );

        f.session_file = Some(PathBuf::from(
            r"C:\Users\me\.claude\projects\C--repo\0b5c-11.jsonl",
        ));
        let r = build(&f, Shell::PowerShell);
        assert!(
            !r.notes.iter().any(|n| n.contains("会话文件在")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn finds_the_session_file_under_any_project() {
        let config = tempfile::tempdir().unwrap();
        let project = config.path().join("projects").join("C--wt-a");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("abc.jsonl"), "{}\n").unwrap();
        assert_eq!(
            find_session_file(config.path(), "abc"),
            Some(project.join("abc.jsonl"))
        );
        assert_eq!(find_session_file(config.path(), "nope"), None);
        // Claude Code's spelling, and CodeBuddy's as found on a real machine.
        assert!(is_project_dir_of(
            "C--Users-me-repo-x",
            Path::new(r"C:\Users\me\repo.x")
        ));
        assert!(is_project_dir_of(
            "c-Users-linkkzhu-.crossagents-chats-01a166d7",
            Path::new(r"C:\Users\linkkzhu\.crossagents\chats\01a166d7")
        ));
        assert!(!is_project_dir_of(
            "C--repo--crossagents-worktrees-ab12-integration",
            Path::new(r"C:\repo")
        ));
    }
}
