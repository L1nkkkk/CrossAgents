//! `crossagents expertise`: the person's and the project's expertise profiles
//! (docs/tasks/EXPERTISE.md).

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use crossagents_api_types::OwnerKind;

use super::Context;
use crate::output::print_json;

#[derive(Debug, Subcommand)]
pub enum ExpertiseCommand {
    /// The profile as Markdown, on stdout.
    Export(ExportArgs),
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Whose profile: `person:me` (the default), `project:<id>`, or a bare `me` / project id.
    #[arg(long, default_value = "person:me")]
    pub owner: String,
    /// Only this domain (`rust`, `gui`, …); every domain when absent.
    #[arg(long)]
    pub domain: Option<String>,
}

pub async fn run(ctx: &Context, cmd: ExpertiseCommand) -> Result<()> {
    match cmd {
        ExpertiseCommand::Export(args) => export(ctx, args).await,
    }
}

async fn export(ctx: &Context, args: ExportArgs) -> Result<()> {
    let (kind, id) = parse_owner(&args.owner)?;
    let mut path = format!(
        "/v1/expertise/export?owner_kind={}&owner_id={}",
        kind_word(kind),
        encode(&id)
    );
    if let Some(domain) = &args.domain {
        path.push_str(&format!("&domain={}", encode(domain)));
    }
    let markdown = ctx.client()?.get_text(&path).await?;
    if ctx.json {
        return print_json(&serde_json::json!({ "markdown": markdown }));
    }
    if markdown.trim().is_empty() {
        // An empty profile exports nothing, so say why instead of printing nothing.
        println!("no expertise entry has been written yet");
        return Ok(());
    }
    print!("{markdown}");
    Ok(())
}

/// `person:me` / `project:<id>`; a bare `me` is the person, anything else a project id.
fn parse_owner(owner: &str) -> Result<(OwnerKind, String)> {
    let owner = owner.trim();
    if let Some(id) = owner.strip_prefix("person:") {
        return Ok((OwnerKind::Person, id.trim().to_owned()));
    }
    if let Some(id) = owner.strip_prefix("project:") {
        if id.trim().is_empty() {
            bail!("--owner project: needs a project id");
        }
        return Ok((OwnerKind::Project, id.trim().to_owned()));
    }
    match owner {
        "" => bail!("--owner cannot be empty"),
        "me" => Ok((OwnerKind::Person, "me".into())),
        id => Ok((OwnerKind::Project, id.to_owned())),
    }
}

fn kind_word(kind: OwnerKind) -> &'static str {
    match kind {
        OwnerKind::Person => "person",
        OwnerKind::Project => "project",
    }
}

/// Percent-encodes a query value: ids and domains are plain, but a domain may not be.
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

    #[test]
    fn the_owner_says_whose_profile_it_is() {
        assert_eq!(
            parse_owner("person:me").unwrap(),
            (OwnerKind::Person, "me".into())
        );
        assert_eq!(parse_owner("me").unwrap(), (OwnerKind::Person, "me".into()));
        assert_eq!(
            parse_owner("project:p1").unwrap(),
            (OwnerKind::Project, "p1".into())
        );
        assert_eq!(
            parse_owner("p1").unwrap(),
            (OwnerKind::Project, "p1".into())
        );
        assert!(parse_owner("project:").is_err());
        assert!(parse_owner("").is_err());
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(encode("me"), "me");
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("测试"), "%E6%B5%8B%E8%AF%95");
    }
}
