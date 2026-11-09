//! How a located CLI is started, and when its prompt cannot be an argument.
//!
//! Cursor on Windows is `agent.cmd`. That file runs `cursor-agent.ps1`, which
//! picks `node.exe` and `index.js` (beside the script, or in the newest
//! `versions\<date-commit>\`). A `.cmd` cannot take a newline in its arguments,
//! so a prompt with one was written to a file. When the pair the script would
//! run can be found, the turn starts those two directly and the prompt stays an
//! argument, newlines included. The CLI does not read the prompt from stdin.
//!
//! [`prompt_via_file`] is the only place that decides a prompt goes in a file.

use std::path::{Path, PathBuf};

use crate::process::resolve_program;

/// Prompts longer than this (UTF-16 units) go in a file: a Windows command line
/// holds 32767, and the other arguments need room too.
pub use crate::with_native::wire::PROMPT_ARG_LIMIT;

/// `node.exe` and `index.js` that `cursor-agent.ps1` would run for the install
/// that contains `program` (`agent.cmd` / `cursor-agent.cmd`). `None` when this
/// is not that layout, or the files the script would run are not both there:
/// the caller then starts `program` itself.
pub fn resolve_cursor_node(program: &Path) -> Option<(PathBuf, PathBuf)> {
    let ext = program.extension().and_then(|ext| ext.to_str())?;
    if !ext.eq_ignore_ascii_case("cmd") && !ext.eq_ignore_ascii_case("bat") {
        return None;
    }
    let root = program.parent()?;
    // The script first runs `versions\promote-staged.cmd`, which moves a downloaded `*.new`
    // version into place. Starting `node.exe` directly would skip that, and the CLI would stay
    // on the old version. While one is staged the launch goes through the script; the next
    // launch, with nothing staged, starts node directly again.
    if has_staged_version(&root.join("versions")) {
        return None;
    }
    // The script runs `node.exe` beside itself and never looks in `versions\`
    // when that file is present, even if `index.js` is missing.
    let beside = root.join("node.exe");
    if beside.is_file() {
        let script = root.join("index.js");
        return script.is_file().then_some((beside, script));
    }
    let mut best: Option<(u32, String, PathBuf)> = None;
    for entry in std::fs::read_dir(root.join("versions")).ok()?.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(date) = version_date(&name) else {
            continue;
        };
        // `Sort-Object { Parse-VersionString }` compares the date only. Equal
        // dates stay in directory order, which is not reproducible, so the
        // directory name breaks the tie: the `HH-MM-SS` form then sorts a later
        // build on the same day first.
        let take = best.as_ref().is_none_or(|(best_date, best_name, _)| {
            (date, name.as_str()) > (*best_date, best_name.as_str())
        });
        if take {
            best = Some((date, name, entry.path()));
        }
    }
    let (_, _, dir) = best?;
    let node = dir.join("node.exe");
    let script = dir.join("index.js");
    (node.is_file() && script.is_file()).then_some((node, script))
}

/// Whether a downloaded version waits in `versions` for the script to promote it.
fn has_staged_version(versions: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(versions) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_dir())
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".new"))
    })
}

/// `Parse-VersionString` plus the directory filter in `cursor-agent.ps1`:
/// `YYYY.M.D-hex` or `YYYY.M.D-HH-MM-SS-hex`, compared as `YYYYMMDD`.
fn version_date(name: &str) -> Option<u32> {
    let (date, rest) = name.split_once('-')?;
    let mut parts = date.split('.');
    let year = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if year.len() != 4 || !ascii_digits(year) {
        return None;
    }
    if !(1..=2).contains(&month.len()) || !ascii_digits(month) {
        return None;
    }
    if !(1..=2).contains(&day.len()) || !ascii_digits(day) {
        return None;
    }
    if !commit_tail(rest) {
        return None;
    }
    let year: u32 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    let day: u32 = day.parse().ok()?;
    Some(year * 10_000 + month * 100 + day)
}

/// The script's `-match` allows a lowercase hex commit, optionally behind a
/// build time `HH-MM-SS`.
fn commit_tail(rest: &str) -> bool {
    if !rest.is_empty() && hex(rest) {
        return true;
    }
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 4
        && two_digits(parts[0])
        && two_digits(parts[1])
        && two_digits(parts[2])
        && !parts[3].is_empty()
        && hex(parts[3])
}

fn ascii_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn two_digits(text: &str) -> bool {
    text.len() == 2 && ascii_digits(text)
}

fn hex(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn command_path(program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if path.is_file() {
        return path;
    }
    let resolved = resolve_program(program);
    if resolved.is_file() { resolved } else { path }
}

/// The program and arguments to start. `turn_args` are the driver's own
/// arguments (`create-chat`, or one turn's flags and prompt). When `command`
/// is Cursor's Windows shim, this is `node.exe index.js` plus the words that
/// followed the `.cmd`. Otherwise it is `command` unchanged, with its program
/// resolved the usual way.
pub fn launch_command(
    command: &[String],
    turn_args: Vec<String>,
) -> Option<(PathBuf, Vec<String>)> {
    let (program, extra) = command.split_first()?;
    if let Some((node, script)) = resolve_cursor_node(&command_path(program)) {
        let mut args = Vec::with_capacity(1 + extra.len() + turn_args.len());
        args.push(script.display().to_string());
        args.extend(extra.iter().cloned());
        args.extend(turn_args);
        return Some((node, args));
    }
    let mut args = Vec::with_capacity(extra.len() + turn_args.len());
    args.extend(extra.iter().cloned());
    args.extend(turn_args);
    Some((resolve_program(program), args))
}

/// Whether `text` has to be handed over in a file.
///
/// A prompt past [`PROMPT_ARG_LIMIT`] always does. A prompt with a newline does
/// only when the launch is still a `.cmd` / `.bat`: Rust will not start one
/// with a newline in its arguments, and `cmd.exe` would mangle the rest.
/// Resolved to `node.exe`, the newline stays in the argument.
pub fn prompt_via_file(program: Option<&str>, text: &str) -> bool {
    if command_line_units(text) > PROMPT_ARG_LIMIT {
        return true;
    }
    if !text.contains(['\r', '\n']) {
        return false;
    }
    let Some(program) = program else {
        return false;
    };
    let path = command_path(program);
    if resolve_cursor_node(&path).is_some() {
        return false;
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

/// The prompt's size on a Windows command line, in UTF-16 units. Quoting escapes it: a `"`
/// becomes `\"`, and every `\` before one is doubled. Both are counted as two units, which
/// over-counts a backslash that is not followed by a quote: the limit then holds for any
/// quoting.
fn command_line_units(text: &str) -> usize {
    text.encode_utf16()
        .map(|unit| {
            if unit == u16::from(b'"') || unit == u16::from(b'\\') {
                2
            } else {
                1
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"").unwrap();
    }

    fn version(root: &Path, name: &str, complete: bool) {
        let dir = root.join("versions").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if complete {
            touch(&dir.join("node.exe"));
            touch(&dir.join("index.js"));
        }
    }

    #[test]
    fn the_script_date_picks_the_version_not_the_name_order() {
        let root = tempfile::tempdir().unwrap();
        let cmd = root.path().join("agent.cmd");
        touch(&cmd);
        // Alphabetical order would prefer `2026.9.30` (`'9' > '1'`). The
        // script's date integer prefers December.
        version(root.path(), "2026.9.30-aaa", true);
        version(root.path(), "2026.12.01-bbb", true);
        version(root.path(), "2026.1.9-ccc", true);
        version(root.path(), "not-a-version", true);
        version(root.path(), "2026.11.05-ABC", true);
        version(root.path(), "pending-launchers", true);
        let (node, script) = resolve_cursor_node(&cmd).unwrap();
        let dir = root.path().join("versions").join("2026.12.01-bbb");
        assert_eq!(node, dir.join("node.exe"));
        assert_eq!(script, dir.join("index.js"));
    }

    #[test]
    fn the_same_day_keeps_the_later_build_time_and_a_copy_beside_the_script_wins() {
        let root = tempfile::tempdir().unwrap();
        let cmd = root.path().join("agent.cmd");
        touch(&cmd);
        version(root.path(), "2026.11.05-01-00-00-aaa", true);
        version(root.path(), "2026.11.05-22-00-00-bbb", true);
        version(root.path(), "2026.11.04-zzz", true);
        let (node, _) = resolve_cursor_node(&cmd).unwrap();
        assert!(
            node.ends_with(Path::new("2026.11.05-22-00-00-bbb").join("node.exe")),
            "{node:?}"
        );

        touch(&root.path().join("node.exe"));
        touch(&root.path().join("index.js"));
        let (node, script) = resolve_cursor_node(&cmd).unwrap();
        assert_eq!(node, root.path().join("node.exe"));
        assert_eq!(script, root.path().join("index.js"));
    }

    #[test]
    fn a_shim_that_does_not_resolve_stays_unresolved() {
        let root = tempfile::tempdir().unwrap();
        let cmd = root.path().join("agent.cmd");
        touch(&cmd);
        assert!(resolve_cursor_node(&cmd).is_none());

        // The newest directory matches the name but has no node. The script
        // would run that one, so an older complete install is not a substitute.
        version(root.path(), "2026.11.05-aaa", false);
        version(root.path(), "2026.10.01-bbb", true);
        assert!(resolve_cursor_node(&cmd).is_none());

        // `node.exe` beside the script with no `index.js` does not fall through.
        touch(&root.path().join("node.exe"));
        assert!(resolve_cursor_node(&cmd).is_none());

        assert!(resolve_cursor_node(Path::new("with-cli.exe")).is_none());
        let (program, args) =
            launch_command(&[cmd.display().to_string()], vec!["create-chat".into()]).unwrap();
        assert_eq!(program, cmd);
        assert_eq!(args, ["create-chat"]);
    }

    #[test]
    fn a_resolved_shim_starts_node_and_keeps_a_newline_out_of_a_file() {
        let root = tempfile::tempdir().unwrap();
        let cmd = root.path().join("agent.cmd");
        touch(&cmd);
        version(root.path(), "2026.11.05-86047df", true);
        let node = root
            .path()
            .join("versions")
            .join("2026.11.05-86047df")
            .join("node.exe");
        let script = root
            .path()
            .join("versions")
            .join("2026.11.05-86047df")
            .join("index.js");
        let (program, args) = launch_command(
            &[cmd.display().to_string(), "--verbose".into()],
            vec!["--".into(), "one\ntwo".into()],
        )
        .unwrap();
        assert_eq!(program, node);
        assert_eq!(
            args,
            vec![
                script.display().to_string(),
                "--verbose".into(),
                "--".into(),
                "one\ntwo".into(),
            ]
        );

        let program = cmd.display().to_string();
        assert!(!prompt_via_file(Some(&program), "one\ntwo"));
        assert!(!prompt_via_file(Some(&program), "plain"));
        let long = "😀".repeat(12_001);
        assert!(long.chars().count() < PROMPT_ARG_LIMIT);
        assert!(prompt_via_file(Some(&program), &long));

        let broken = root.path().join("empty").join("other.cmd");
        touch(&broken);
        let broken = broken.display().to_string();
        assert!(prompt_via_file(Some(&broken), "one\ntwo"));
        assert!(!prompt_via_file(Some(&broken), "plain"));
        assert!(!prompt_via_file(Some("with-cli.exe"), "one\ntwo"));
        assert!(launch_command(&[], vec![]).is_none());
    }

    #[test]
    fn a_staged_version_goes_through_the_script_until_it_is_promoted() {
        let root = tempfile::tempdir().unwrap();
        let cmd = root.path().join("agent.cmd");
        touch(&cmd);
        version(root.path(), "2026.11.05-86047df", true);
        assert!(resolve_cursor_node(&cmd).is_some());

        // A downloaded version waits in `versions`: the script promotes it before it
        // picks one, so node must not be started directly this time.
        let staged = root.path().join("versions").join("2026.11.06-bbbbbbb.new");
        std::fs::create_dir_all(&staged).unwrap();
        assert!(resolve_cursor_node(&cmd).is_none());
        let (program, args) =
            launch_command(&[cmd.display().to_string()], vec!["create-chat".into()]).unwrap();
        assert_eq!(program, cmd);
        assert_eq!(args, ["create-chat"]);

        // Promoted (the script renamed it): node is started directly again, and the
        // new version is the one it picks.
        let promoted = root.path().join("versions").join("2026.11.06-bbbbbbb");
        std::fs::rename(&staged, &promoted).unwrap();
        touch(&promoted.join("node.exe"));
        touch(&promoted.join("index.js"));
        let (node, script) = resolve_cursor_node(&cmd).unwrap();
        assert_eq!(node, promoted.join("node.exe"));
        assert_eq!(script, promoted.join("index.js"));
    }

    #[test]
    fn a_staged_file_is_not_a_version_to_promote() {
        let root = tempfile::tempdir().unwrap();
        let cmd = root.path().join("agent.cmd");
        touch(&cmd);
        version(root.path(), "2026.11.05-86047df", true);
        touch(&root.path().join("versions").join("2026.11.06-bbbbbbb.new"));
        assert!(resolve_cursor_node(&cmd).is_some());
    }

    #[test]
    fn quotes_count_twice_because_quoting_escapes_them() {
        // 13,000 quotes are 13,000 UTF-16 units, but 26,000 on the command line.
        let quotes = "\"".repeat(13_000);
        assert!(quotes.encode_utf16().count() < PROMPT_ARG_LIMIT);
        assert!(prompt_via_file(Some("with-cli.exe"), &quotes));
        assert!(!prompt_via_file(Some("with-cli.exe"), &"\"".repeat(12_000)));
        assert!(prompt_via_file(None, &"\\".repeat(12_001)));
        assert!(!prompt_via_file(None, &"a".repeat(PROMPT_ARG_LIMIT)));
    }
}
