//! Stream rules (docs/tasks/STREAM-RULES.md): rules kept out of the agent's context until its
//! output or a tool call matches one. Then the hub puts the rule into the running turn (a
//! steer), or interrupts the turn and runs the rule next when the driver cannot steer.
//!
//! A rule is sent at most once in [`DEDUP_TURNS`] turns of a session; compaction forgets
//! what was sent. Rules come from [`RuleSource`]s: the project's rule files are the first.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use regex::Regex;

/// Turns in which a rule, once sent, is not sent again: a rule sent in turn `t` is held back
/// through turn `t + DEDUP_TURNS - 1`.
pub const DEDUP_TURNS: u64 = 20;

/// Bytes of the turn's output kept, so a pattern can match text that spans two chunks.
const WINDOW: usize = 4096;

/// Where a rule applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum On {
    /// The agent's text output.
    Output,
    /// A tool call: its name, its arguments.
    Tool,
    Both,
}

impl On {
    fn covers_output(self) -> bool {
        matches!(self, On::Output | On::Both)
    }

    fn covers_tool(self) -> bool {
        matches!(self, On::Tool | On::Both)
    }
}

/// One rule, parsed and with its patterns compiled.
#[derive(Debug)]
pub struct Rule {
    pub id: String,
    pub patterns: Vec<Regex>,
    pub on: On,
    /// The text sent to the agent.
    pub body: String,
}

impl Rule {
    fn matches(&self, text: &str) -> bool {
        self.patterns.iter().any(|pattern| pattern.is_match(text))
    }

    /// The message that carries the rule into the turn; it says the text is a rule.
    pub fn message(&self) -> String {
        format!(
            "CrossAgents rule `{}` (a stream rule; it applies from now on):\n\n{}",
            self.id, self.body
        )
    }
}

/// Parses one rule file: a `---` block of `key: value` lines, then the text to send. `stem`
/// is the file name without `.md`, the id when the file gives none.
///
/// Keys: `id`; `match` (a regular expression, repeatable, or a `- ` list under a bare
/// `match:`); `on` (`output` (default), `tool` or `both`). Quotes around a value are
/// dropped, nothing else is unescaped. Other keys are ignored.
pub fn parse(stem: &str, text: &str) -> Result<Rule, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("no frontmatter: the file must start with a `---` line".into());
    }
    let mut id = None;
    let mut sources: Vec<String> = Vec::new();
    let mut on = On::Output;
    let mut in_list = false;
    let mut closed = false;
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed == "---" {
            closed = true;
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            if !in_list {
                return Err(format!("a list item outside `match`: {trimmed}"));
            }
            sources.push(unquote(item));
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            return Err(format!("not a `key: value` line: {trimmed}"));
        };
        let value = value.trim();
        in_list = false;
        match key.trim() {
            "id" => id = Some(unquote(value)),
            "match" => {
                if value.is_empty() {
                    in_list = true;
                } else {
                    sources.push(unquote(value));
                }
            }
            "on" => {
                on = match value {
                    "output" => On::Output,
                    "tool" => On::Tool,
                    "both" => On::Both,
                    other => {
                        return Err(format!("`on` must be output, tool or both, not `{other}`"));
                    }
                }
            }
            _ => {}
        }
    }
    if !closed {
        return Err("the frontmatter is not closed by a `---` line".into());
    }
    if sources.is_empty() {
        return Err("no `match` pattern".into());
    }
    let patterns = sources
        .iter()
        .map(|source| Regex::new(source).map_err(|err| format!("bad `match` `{source}`: {err}")))
        .collect::<Result<Vec<_>, _>>()?;
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    if body.is_empty() {
        return Err("nothing to send: the text after the frontmatter is empty".into());
    }
    let id = id
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| stem.to_owned());
    Ok(Rule {
        id,
        patterns,
        on,
        body,
    })
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    value.to_owned()
}

/// Where rules come from. The project's rule files are the first source; an extension adds
/// its own by implementing this trait and passing it to [`load_from`].
pub trait RuleSource: Send + Sync {
    /// The source's name, for warnings.
    fn name(&self) -> &str;

    /// The rules for an agent working in `cwd`. What cannot be read or parsed is logged and
    /// left out: a source never stops a session from starting.
    fn rules(&self, cwd: &Path) -> Vec<Rule>;
}

/// `.crossagents/rules/*.md` at the repository's root (or in `cwd`, outside a repository).
pub struct ProjectRuleFiles;

impl RuleSource for ProjectRuleFiles {
    fn name(&self) -> &str {
        "project rule files"
    }

    fn rules(&self, cwd: &Path) -> Vec<Rule> {
        let dir = repo_root(cwd).join(".crossagents").join("rules");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "cannot read the stream rules directory; no stream rules");
                return Vec::new();
            }
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
            .collect();
        paths.sort();
        paths
            .iter()
            .filter_map(|path| {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                let text = match std::fs::read_to_string(path) {
                    Ok(text) => text,
                    Err(err) => {
                        tracing::warn!(file = %path.display(), error = %err, "cannot read the stream rule; skipped");
                        return None;
                    }
                };
                match parse(stem, &text) {
                    Ok(rule) => Some(rule),
                    Err(why) => {
                        tracing::warn!(file = %path.display(), %why, "stream rule skipped");
                        None
                    }
                }
            })
            .collect()
    }
}

/// The nearest directory from `cwd` up that holds a `.git`, else `cwd` itself.
fn repo_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// The stream rules for an agent working in `cwd`, from the project's rule files and, when
/// the home is known, those of the enabled extensions under it.
pub fn load(cwd: &Path, home: Option<&Path>) -> Vec<Rule> {
    let project = ProjectRuleFiles;
    let extensions = home.map(crate::extensions::rules::ExtensionRuleFiles::new);
    let mut sources: Vec<&dyn RuleSource> = vec![&project];
    if let Some(extensions) = &extensions {
        sources.push(extensions);
    }
    load_from(&sources, cwd)
}

/// The rules of every source, in order. A rule whose id an earlier one already has is
/// skipped (with a warning).
pub fn load_from(sources: &[&dyn RuleSource], cwd: &Path) -> Vec<Rule> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for source in sources {
        for rule in source.rules(cwd) {
            if seen.insert(rule.id.clone()) {
                out.push(rule);
            } else {
                tracing::warn!(rule = %rule.id, source = source.name(), "stream rule with a repeated id skipped");
            }
        }
    }
    out
}

/// A rule to send now: its id and the message that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Injection {
    pub rule: String,
    pub text: String,
}

/// One session's stream rules: what they match, and when each was last sent.
#[derive(Debug, Default)]
pub struct StreamRules {
    rules: Vec<Rule>,
    /// Turns started in the session.
    turn: u64,
    /// The turn each rule (by index) was last sent in; empty after a compaction.
    sent: HashMap<usize, u64>,
    /// The rules sent in the turn that is running now; cleared when it ends. A compaction
    /// inside a turn forgets what earlier turns sent, but not what this turn already did.
    sent_now: HashSet<usize>,
    /// The end of the turn's output, for patterns across chunks.
    window: String,
}

impl StreamRules {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn turn_started(&mut self) {
        self.turn += 1;
        self.window.clear();
        self.sent_now.clear();
    }

    /// The turn ended: a compaction between turns is not "inside" the next one.
    pub fn turn_finished(&mut self) {
        self.sent_now.clear();
    }

    /// The session's context was compacted: what was sent is no longer in it, so the rules
    /// may be sent again. A compaction in the middle of a turn keeps what that turn already
    /// sent (it cleared on [`Self::turn_finished`]): one rule, one send a turn.
    ///
    /// Told by the hub when its own compaction succeeds, and by the driver when the agent
    /// compacts by itself (`SessionEventKind::Compacted`).
    pub fn compacted(&mut self) {
        self.sent.clear();
    }

    /// Output text arrived; returns the rules to send now.
    pub fn on_output(&mut self, text: &str) -> Vec<Injection> {
        self.window.push_str(text);
        if self.window.len() > WINDOW {
            let mut cut = self.window.len() - WINDOW;
            while !self.window.is_char_boundary(cut) {
                cut += 1;
            }
            self.window = self.window[cut..].to_owned();
        }
        let hits: Vec<usize> = {
            let window = &self.window;
            self.rules
                .iter()
                .enumerate()
                .filter(|(_, rule)| rule.on.covers_output() && rule.matches(window))
                .map(|(index, _)| index)
                .collect()
        };
        self.send(hits)
    }

    /// A tool call's text (its name, its arguments) arrived; returns the rules to send now.
    pub fn on_tool(&mut self, text: &str) -> Vec<Injection> {
        let hits: Vec<usize> = self
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.on.covers_tool() && rule.matches(text))
            .map(|(index, _)| index)
            .collect();
        self.send(hits)
    }

    /// The matching rules that are due, marked as sent in this turn.
    fn send(&mut self, hits: Vec<usize>) -> Vec<Injection> {
        let mut out = Vec::new();
        for index in hits {
            if self.sent_now.contains(&index) {
                continue;
            }
            let due = self
                .sent
                .get(&index)
                .is_none_or(|&turn| self.turn.saturating_sub(turn) >= DEDUP_TURNS);
            if !due {
                continue;
            }
            self.sent.insert(index, self.turn);
            self.sent_now.insert(index);
            let rule = &self.rules[index];
            out.push(Injection {
                rule: rule.id.clone(),
                text: rule.message(),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(text: &str) -> Rule {
        parse("sample", text).expect("the rule parses")
    }

    #[test]
    fn frontmatter_gives_id_patterns_and_body() {
        let r = rule("---\nmatch: sleep\nmatch: \"foo.*bar\"\non: both\n---\n\nSlow down.\n");
        assert_eq!(r.id, "sample", "the file name is the default id");
        assert_eq!(r.on, On::Both);
        assert_eq!(r.patterns.len(), 2);
        assert_eq!(r.body, "Slow down.");
    }

    #[test]
    fn id_and_list_patterns_are_read() {
        let r = rule("---\nid: no-rm\nmatch:\n  - rm -rf\n  - 'drop table'\n---\nDon't.\n");
        assert_eq!(r.id, "no-rm");
        assert_eq!(r.on, On::Output, "output is the default");
        assert!(r.patterns[1].is_match("drop table x"));
        assert_eq!(r.patterns.len(), 2);
    }

    #[test]
    fn bad_files_are_errors_not_panics() {
        for text in [
            "match: x\n---\nbody\n",                  // no opening fence
            "---\nmatch: x\nbody\n",                  // not closed
            "---\nbody\n---\nbody\n",                 // no match
            "---\nmatch: (\n---\nbody\n",             // bad regex
            "---\nmatch: x\non: sometimes\n---\nb\n", // bad on
            "---\nmatch: x\n---\n  \n",               // empty body
        ] {
            assert!(parse("s", text).is_err(), "accepted: {text:?}");
        }
    }

    #[test]
    fn a_source_skips_bad_files_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = dir.path().join(".crossagents").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("a-good.md"), "---\nmatch: ok\n---\nGood.\n").unwrap();
        std::fs::write(rules_dir.join("b-bad.md"), "---\nmatch: (\n---\nBad.\n").unwrap();
        std::fs::write(rules_dir.join("c-nofm.md"), "just text\n").unwrap();
        std::fs::write(
            rules_dir.join("notes.txt"),
            "---\nmatch: x\n---\nIgnored.\n",
        )
        .unwrap();
        let loaded = load(dir.path(), None);
        let ids: Vec<&str> = loaded.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a-good"]);
    }

    #[test]
    fn a_missing_rules_directory_is_no_rules() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path(), None).is_empty());
    }

    #[test]
    fn repeated_ids_keep_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let rules_dir = dir.path().join(".crossagents").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("x.md"),
            "---\nid: same\nmatch: a\n---\nFirst.\n",
        )
        .unwrap();
        std::fs::write(
            rules_dir.join("y.md"),
            "---\nid: same\nmatch: b\n---\nSecond.\n",
        )
        .unwrap();
        let loaded = load(dir.path(), None);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].body, "First.");
    }

    fn one(text: &str) -> StreamRules {
        StreamRules::new(vec![rule(text)])
    }

    #[test]
    fn a_match_sends_the_rule_once_in_twenty_turns() {
        let mut s = one("---\nmatch: sleeping\n---\nRule body.\n");
        s.turn_started();
        let sent = s.on_output("sleeping ");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].rule, "sample");
        assert!(sent[0].text.contains("Rule body."));
        assert!(sent[0].text.contains("`sample`"));
        // Turns 2 to 20 are inside the window of the first send.
        for _ in 2..=DEDUP_TURNS {
            s.turn_started();
            assert!(s.on_output("sleeping again").is_empty());
        }
        // Turn 21 is the first one past it.
        s.turn_started();
        assert_eq!(s.on_output("sleeping").len(), 1);
    }

    #[test]
    fn a_second_match_in_the_same_turn_sends_nothing() {
        let mut s = one("---\nmatch: sleeping\non: both\n---\nBody.\n");
        s.turn_started();
        assert_eq!(s.on_output("sleeping").len(), 1);
        assert!(s.on_output("sleeping").is_empty());
        assert!(s.on_tool("sleeping").is_empty());
    }

    #[test]
    fn compaction_makes_the_rule_due_again() {
        let mut s = one("---\nmatch: sleeping\n---\nBody.\n");
        s.turn_started();
        assert_eq!(s.on_output("sleeping").len(), 1);
        // The turn ends, then the hub compacts: everything is due again.
        s.turn_finished();
        s.compacted();
        s.turn_started();
        assert_eq!(
            s.on_output("sleeping").len(),
            1,
            "sent again after compaction"
        );
    }

    #[test]
    fn a_compaction_inside_a_turn_does_not_send_the_rule_twice_in_it() {
        let mut s = one("---\nmatch: sleeping\n---\nBody.\n");
        s.turn_started();
        assert_eq!(s.on_output("sleeping").len(), 1);
        // The agent compacts by itself while the turn runs; the rule matched again after.
        s.compacted();
        assert!(
            s.on_output("sleeping").is_empty(),
            "one send a turn, compaction or not"
        );
        s.turn_finished();
        s.turn_started();
        assert_eq!(
            s.on_output("sleeping").len(),
            1,
            "the compaction still lets it go in a later turn"
        );
    }

    #[test]
    fn a_pattern_spans_chunks() {
        let mut s = one("---\nmatch: SPAN\n---\nBody.\n");
        s.turn_started();
        assert!(s.on_output("SP").is_empty());
        assert_eq!(s.on_output("AN").len(), 1);
    }

    #[test]
    fn the_window_resets_each_turn() {
        let mut s = one("---\nmatch: SPAN\n---\nBody.\n");
        s.turn_started();
        assert!(s.on_output("SP").is_empty());
        s.turn_started();
        assert!(
            s.on_output("AN").is_empty(),
            "the first half was another turn's"
        );
    }

    #[test]
    fn output_and_tool_rules_see_their_own_text() {
        let mut s = StreamRules::new(vec![
            rule("---\nid: out\nmatch: alpha\n---\nOut.\n"),
            rule("---\nid: tool\nmatch: rm\non: tool\n---\nTool.\n"),
        ]);
        s.turn_started();
        assert!(
            s.on_tool("alpha").is_empty(),
            "output-only rule ignores tools"
        );
        assert_eq!(s.on_tool("Bash rm -rf").len(), 1);
        assert!(
            s.on_output("rm").is_empty(),
            "tool-only rule ignores output"
        );
        let sent = s.on_output("alpha");
        assert_eq!(
            sent.iter().map(|i| i.rule.as_str()).collect::<Vec<_>>(),
            ["out"]
        );
    }

    #[test]
    fn nothing_loaded_is_empty() {
        assert!(StreamRules::new(Vec::new()).is_empty());
    }
}
