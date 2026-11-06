//! `[[rules]]`: stream rules an enabled extension ships (docs/EXTENSIONS.md §4).
//!
//! A rule is a file in the approved snapshot, in the same format as the project's
//! `.crossagents/rules/*.md`; the hub reads it when an agent's session starts. Only the
//! current (approved) snapshot is read, never the source directory, so an extension's rules
//! do not take effect before a person approved that version (docs/EXTENSIONS.md §6).

use std::path::{Path, PathBuf};

use crate::stream_rules::{parse, Rule, RuleSource};

/// A rule file's size limit: a rule is a few lines of guidance, not a document.
const RULE_LIMIT: u64 = 64 * 1024;

/// The `[[rules]]` of every enabled extension under a CrossAgents home.
pub struct ExtensionRuleFiles {
    home: PathBuf,
}

impl ExtensionRuleFiles {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }
}

impl RuleSource for ExtensionRuleFiles {
    fn name(&self) -> &str {
        "extension rules"
    }

    fn rules(&self, _cwd: &Path) -> Vec<Rule> {
        let mut out = Vec::new();
        for (name, dir) in super::registry::enabled_snapshots(&self.home) {
            let manifest = match super::manifest::Manifest::read(&dir, &name) {
                Ok(manifest) => manifest,
                Err(err) => {
                    tracing::warn!(
                        extension = %name,
                        dir = %dir.display(),
                        error = %err,
                        "cannot read the extension's description; none of its stream rules"
                    );
                    continue;
                }
            };
            for declared in &manifest.rules {
                match read_rule(&dir, &name, &declared.file) {
                    Ok(rule) => out.push(rule),
                    Err(why) => tracing::warn!(
                        extension = %name,
                        file = %declared.file,
                        %why,
                        "extension stream rule skipped"
                    ),
                }
            }
        }
        out
    }
}

/// One declared rule file, parsed as a project rule would be. Its id is prefixed with the
/// extension's name, so two extensions can ship the same rule name.
fn read_rule(dir: &Path, extension: &str, file: &str) -> Result<Rule, String> {
    let path = dir.join(file);
    let meta = std::fs::metadata(&path).map_err(|err| format!("cannot read the rule file: {err}"))?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > RULE_LIMIT {
        return Err(format!("larger than {RULE_LIMIT} bytes"));
    }
    let text =
        std::fs::read_to_string(&path).map_err(|err| format!("cannot read the rule file: {err}"))?;
    let stem = Path::new(file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("rule");
    let mut rule = parse(stem, &text)?;
    rule.id = format!("{extension}/{}", rule.id);
    Ok(rule)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A home with one enabled extension whose snapshot holds the given files.
    fn home(extension: &str, enabled: bool, files: &[(&str, &str)]) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let fingerprint = "a".repeat(64);
        let dir = home
            .path()
            .join("extension-versions")
            .join(extension)
            .join(&fingerprint);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("extension.toml"),
            format!(
                "name = \"{extension}\"\napi = 1\naccess = \"read\"\n\n[[rules]]\nfile = \"rules/one.md\"\n"
            ),
        )
        .unwrap();
        for (path, text) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fs::write(
            home.path().join("extensions.json"),
            format!(
                "{{\"{extension}\": {{ \"enabled\": {enabled}, \"current\": \"{fingerprint}\" }}}}"
            ),
        )
        .unwrap();
        home
    }

    #[test]
    fn an_enabled_extensions_rules_are_read_with_its_name_in_the_id() {
        let home = home(
            "standup",
            true,
            &[(
                "rules/one.md",
                "---\nid: no-sleep\nmatch: sleeping\n---\nPoll instead.\n",
            )],
        );
        let rules = ExtensionRuleFiles::new(home.path()).rules(Path::new("."));
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "standup/no-sleep");
        assert_eq!(rules[0].body, "Poll instead.");
        assert!(rules[0].patterns[0].is_match("sleeping"));
    }

    #[test]
    fn a_disabled_extension_contributes_nothing() {
        let home = home(
            "standup",
            false,
            &[(
                "rules/one.md",
                "---\nmatch: sleeping\n---\nPoll instead.\n",
            )],
        );
        assert!(
            ExtensionRuleFiles::new(home.path())
                .rules(Path::new("."))
                .is_empty()
        );
    }

    #[test]
    fn a_bad_rule_file_is_skipped_and_the_others_kept() {
        let home = home(
            "standup",
            true,
            &[
                ("rules/one.md", "no frontmatter\n"),
                ("rules/two.md", "---\nmatch: (\n---\nBad regex.\n"),
            ],
        );
        assert!(
            ExtensionRuleFiles::new(home.path())
                .rules(Path::new("."))
                .is_empty(),
            "an unreadable rule is logged, not fatal"
        );
    }

    #[test]
    fn a_home_without_extensions_json_has_no_rules() {
        let home = tempfile::tempdir().unwrap();
        assert!(
            ExtensionRuleFiles::new(home.path())
                .rules(Path::new("."))
                .is_empty()
        );
    }

    #[test]
    fn an_extension_whose_rule_file_is_missing_contributes_nothing() {
        // The manifest declares `rules/one.md`; the snapshot has no such file, so the
        // manifest does not read: logged, and the session gets no rules from it.
        let home = home("standup", true, &[]);
        assert!(
            ExtensionRuleFiles::new(home.path())
                .rules(Path::new("."))
                .is_empty()
        );
    }
}
