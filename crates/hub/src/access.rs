//! Who may call what (docs/API-AUTH.md §3, docs/tasks/DOGFOOD.md §7): credentials have a
//! level, routes require one. The routes say nothing about credentials; A1 (users and
//! devices) replaces how a credential gets its level, not this table.

use axum::http::Method;
use crossagents_api_types::ExtensionAccess;

/// What a credential may do; each level includes the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Read,
    Write,
    Admin,
}

impl Level {
    pub fn word(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

/// What a presented token turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    Level(Level),
    /// An agent's session token: it only reaches `/v1/me*`.
    Agent,
    /// The token the hub issued for one run of an extension tool (docs/EXTENSIONS.md §3). It
    /// is read or write, never admin, and it names the extension so the log can.
    Extension {
        name: String,
        level: ExtensionAccess,
    },
}

impl Grant {
    /// The level this extension token has. Admin is not representable.
    fn extension_level(level: ExtensionAccess) -> Level {
        match level {
            ExtensionAccess::Read => Level::Read,
            ExtensionAccess::Write => Level::Write,
        }
    }
}

/// The route that calls an extension tool as the person who clicked (docs/EXTENSIONS.md §3).
pub const EXTENSION_CALL_ROUTE: &str = "/v1/extensions/{name}/tools/{tool}";

/// The level `method` on the route `pattern` (as the router names it, e.g.
/// `/v1/accounts/{id}`) requires: reading is read; changing is write, except what touches
/// credentials or the hub's life, which is admin.
pub fn required(method: &Method, pattern: &str) -> Level {
    let admin = matches!(
        (method.as_str(), pattern),
        ("POST", "/v1/shutdown")
            | ("POST", "/v1/accounts")
            | ("POST", "/v1/accounts/import")
            | ("DELETE", "/v1/accounts/{id}")
            | ("POST", "/v1/accounts/{id}/login")
            // They run a program on this machine (docs/DESIGN.md §9.3).
            | ("POST", "/v1/plugins/{name}/install")
            | ("POST", "/v1/plugins/{name}/upgrade")
            // It runs the CLIs' version commands, as those do.
            | ("POST", "/v1/plugins/refresh")
    ) || is_terminal(pattern)
        || is_extension_trust(method, pattern);
    if admin {
        Level::Admin
    } else if *method == Method::GET || reads_only(pattern) {
        Level::Read
    } else {
        Level::Write
    }
}

/// A terminal runs what is typed into it as this machine's user, and reading one shows whatever
/// that printed, tokens included: every route, reading too, is admin.
fn is_terminal(pattern: &str) -> bool {
    pattern == "/v1/terminals" || pattern.starts_with("/v1/terminals/")
}

/// Granting an extension trust is admin, and only a person reaches it (docs/EXTENSIONS.md
/// §6): enabling a snapshot makes the hub run code on this machine as this user, and
/// auto-trust makes it do so again on every change without asking anyone.
///
/// `disable` is not here: taking trust away only ever narrows what runs, and the panel's
/// own "disable" button (the error boundary, §5) must not need more than the rest of the
/// GUI. So it is write, like any other change, and re-enabling still costs admin. Reading
/// the listing and the pending diff is read; calling a tool of an extension a person
/// already trusted is write.
fn is_extension_trust(method: &Method, pattern: &str) -> bool {
    matches!(
        (method.as_str(), pattern),
        ("POST", "/v1/extensions/{name}/enable") | ("POST", "/v1/extensions/{name}/auto-trust")
    )
}

/// POSTs that only read: `files/resolve` answers which paths name something inside a
/// workspace, the paths carried in the request (docs/tasks/DOGFOOD.md §7).
fn reads_only(pattern: &str) -> bool {
    pattern.ends_with("/files/resolve")
}

/// `None` when `grant` may call the route, else why not (for a 403).
pub fn refusal(grant: &Grant, method: &Method, pattern: &str) -> Option<String> {
    let needed = required(method, pattern);
    match grant {
        Grant::Agent => Some(format!(
            "an agent's session token reaches /v1/me only; {method} {pattern} needs the {} level",
            needed.word()
        )),
        // Only a person's credential is a human caller. The route fills `caller` with
        // `{ kind: "human" }`, which opens the tools declared for `human`; an extension that
        // could reach it would call them as a person (docs/EXTENSIONS.md §3).
        Grant::Extension { name, .. }
            if method == Method::POST && pattern == EXTENSION_CALL_ROUTE =>
        {
            Some(format!(
                "the token of extension {name} cannot call {method} {pattern}: that route calls a tool as a person, and an extension is not one"
            ))
        }
        Grant::Extension { name, level } => {
            let have = Grant::extension_level(*level);
            (have < needed).then(|| {
                format!(
                    "the token of extension {name} has the {} level; {method} {pattern} needs {}",
                    have.word(),
                    needed.word()
                )
            })
        }
        Grant::Level(level) if *level < needed => Some(format!(
            "this credential has the {} level; {method} {pattern} needs {}",
            level.word(),
            needed.word()
        )),
        Grant::Level(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_api_auth_table() {
        assert_eq!(required(&Method::GET, "/v1/agents"), Level::Read);
        assert_eq!(required(&Method::GET, "/v1/accounts/{id}"), Level::Read);
        assert_eq!(
            required(&Method::POST, "/v1/agents/{key}/messages"),
            Level::Write
        );
        assert_eq!(required(&Method::DELETE, "/v1/agents/{key}"), Level::Write);
        assert_eq!(required(&Method::POST, "/v1/shutdown"), Level::Admin);
        assert_eq!(
            required(&Method::POST, "/v1/accounts/{id}/login"),
            Level::Admin
        );
        // Importing copies a login token into a new account: it touches credentials.
        assert_eq!(required(&Method::POST, "/v1/accounts/import"), Level::Admin);
        assert_eq!(required(&Method::GET, "/v1/accounts/import"), Level::Read);
        assert_eq!(required(&Method::DELETE, "/v1/accounts/{id}"), Level::Admin);
        assert_eq!(
            required(&Method::POST, "/v1/plugins/{name}/install"),
            Level::Admin
        );
        assert_eq!(
            required(&Method::POST, "/v1/plugins/{name}/upgrade"),
            Level::Admin
        );
        // Looking for the CLIs again runs their version commands.
        assert_eq!(required(&Method::POST, "/v1/plugins/refresh"), Level::Admin);
        // Asking which paths are inside a workspace reads; nothing changes.
        assert_eq!(
            required(&Method::POST, "/v1/agents/{key}/files/resolve"),
            Level::Read
        );
        assert_eq!(
            required(&Method::POST, "/v1/runs/{key}/tasks/{task}/files/resolve"),
            Level::Read
        );
        assert_eq!(
            required(&Method::GET, "/v1/runs/{key}/files/content"),
            Level::Read
        );
    }

    #[test]
    fn every_terminal_route_is_admin_even_reading() {
        for (method, pattern) in [
            (Method::GET, "/v1/terminals"),
            (Method::POST, "/v1/terminals"),
            (Method::DELETE, "/v1/terminals/{id}"),
            (Method::GET, "/v1/terminals/{id}/output"),
            (Method::POST, "/v1/terminals/{id}/input"),
            (Method::POST, "/v1/terminals/{id}/resize"),
            (Method::POST, "/v1/terminals/{id}/takeover"),
        ] {
            assert_eq!(
                required(&method, pattern),
                Level::Admin,
                "{method} {pattern}"
            );
        }
    }

    /// docs/EXTENSIONS.md §5, §6: trusting a version is admin, calling a trusted tool is
    /// write, reading is read.
    #[test]
    fn extensions_routes_split_trust_from_use() {
        for (method, pattern) in [
            (Method::POST, "/v1/extensions/{name}/enable"),
            (Method::POST, "/v1/extensions/{name}/auto-trust"),
        ] {
            assert_eq!(
                required(&method, pattern),
                Level::Admin,
                "{method} {pattern}"
            );
        }

        assert_eq!(
            required(&Method::GET, "/v1/extensions/{name}/logs"),
            Level::Read
        );
        assert!(
            refusal(
                &Grant::Level(Level::Read),
                &Method::GET,
                "/v1/extensions/{name}/logs"
            )
            .is_none()
        );
        assert!(refusal(&Grant::Agent, &Method::GET, "/v1/extensions/{name}/logs").is_some());
        assert!(refusal(&Grant::Agent, &Method::POST, "/v1/extensions/{name}/enable").is_some());
        // Reading what is installed, and what a pending snapshot would change.
        assert_eq!(required(&Method::GET, "/v1/extensions"), Level::Read);
        assert_eq!(
            required(&Method::GET, "/v1/extensions/{name}/pending/diff"),
            Level::Read
        );
        assert_eq!(
            required(&Method::GET, "/v1/extensions/calls/{id}"),
            Level::Read
        );

        // Using one, and taking trust away, are ordinary changes.
        assert_eq!(
            required(&Method::POST, "/v1/extensions/{name}/tools/{tool}"),
            Level::Write
        );
        assert_eq!(
            required(&Method::POST, "/v1/extensions/{name}/disable"),
            Level::Write
        );
        assert_eq!(
            required(&Method::DELETE, "/v1/extensions/calls/{id}"),
            Level::Write
        );

        // A read-only credential cannot call a tool, and a write one cannot grant trust.
        let read = Grant::Level(Level::Read);
        assert!(
            refusal(&read, &Method::POST, "/v1/extensions/{name}/tools/{tool}")
                .unwrap()
                .contains("needs write")
        );
        assert_eq!(refusal(&read, &Method::GET, "/v1/extensions"), None);
        assert!(
            refusal(
                &Grant::Level(Level::Write),
                &Method::POST,
                "/v1/extensions/{name}/enable"
            )
            .unwrap()
            .contains("needs admin")
        );
        assert_eq!(
            refusal(
                &Grant::Level(Level::Admin),
                &Method::POST,
                "/v1/extensions/{name}/enable"
            ),
            None
        );
        // An agent's session token reaches /v1/me only: no extension route, so no agent can
        // approve a version or flip auto-trust.
        for (method, pattern) in [
            (Method::GET, "/v1/extensions"),
            (Method::POST, "/v1/extensions/{name}/enable"),
            (Method::POST, "/v1/extensions/{name}/auto-trust"),
            (Method::POST, "/v1/extensions/{name}/tools/{tool}"),
        ] {
            assert!(
                refusal(&Grant::Agent, &method, pattern).is_some(),
                "{method} {pattern}"
            );
        }
    }

    /// docs/EXTENSIONS.md §3, §6: an extension's token is read or write and never reaches
    /// the route that calls a tool as a person.
    #[test]
    fn extensions_token_is_capped_and_cannot_pose_as_a_person() {
        let ext = |level| Grant::Extension {
            name: "standup".into(),
            level,
        };
        let (read, write) = (ext(ExtensionAccess::Read), ext(ExtensionAccess::Write));
        // The level it declared, no more.
        assert_eq!(refusal(&read, &Method::GET, "/v1/agents"), None);
        assert_eq!(refusal(&write, &Method::POST, "/v1/runs"), None);
        let why = refusal(&read, &Method::POST, "/v1/runs").unwrap();
        assert!(
            why.contains("standup") && why.contains("needs write"),
            "{why}"
        );
        // Admin is out of reach at either level: trust, shutdown, accounts, terminals.
        for (method, pattern) in [
            (Method::POST, "/v1/extensions/{name}/enable"),
            (Method::POST, "/v1/extensions/{name}/auto-trust"),
            (Method::POST, "/v1/shutdown"),
            (Method::POST, "/v1/accounts"),
            (Method::POST, "/v1/accounts/import"),
            (Method::GET, "/v1/terminals"),
        ] {
            let why = refusal(&write, &method, pattern).expect(pattern);
            assert!(
                why.contains("standup") && why.contains("needs admin"),
                "{why}"
            );
        }
        // Calling a tool as a person is refused whatever the level: a write token passes the
        // level check on this route, so the route itself is what stops it.
        assert_eq!(required(&Method::POST, EXTENSION_CALL_ROUTE), Level::Write);
        let why = refusal(&write, &Method::POST, EXTENSION_CALL_ROUTE).unwrap();
        assert!(why.contains("standup") && why.contains("not one"), "{why}");
        assert!(refusal(&read, &Method::POST, EXTENSION_CALL_ROUTE).is_some());
        // A person's write credential does reach it.
        assert_eq!(
            refusal(
                &Grant::Level(Level::Write),
                &Method::POST,
                EXTENSION_CALL_ROUTE
            ),
            None
        );
        // Looking at a call's result or cancelling one is not posing as anybody.
        assert_eq!(
            refusal(&read, &Method::GET, "/v1/extensions/calls/{id}"),
            None
        );
        assert_eq!(
            refusal(&write, &Method::DELETE, "/v1/extensions/calls/{id}"),
            None
        );
    }

    #[test]
    fn refusals_name_the_missing_level() {
        let read = Grant::Level(Level::Read);
        assert_eq!(refusal(&read, &Method::GET, "/v1/runs"), None);
        let why = refusal(&read, &Method::POST, "/v1/runs").unwrap();
        assert!(
            why.contains("read level") && why.contains("needs write"),
            "{why}"
        );
        assert!(
            refusal(&Grant::Level(Level::Write), &Method::POST, "/v1/shutdown")
                .unwrap()
                .contains("needs admin")
        );
        assert_eq!(
            refusal(&Grant::Level(Level::Admin), &Method::POST, "/v1/shutdown"),
            None
        );
        assert!(refusal(&Grant::Agent, &Method::GET, "/v1/agents").is_some());
    }

    /// docs/TODO.md §14: the Git view only reads, so the read-only credential reaches it.
    #[test]
    fn the_git_view_is_read_only() {
        for pattern in [
            "/v1/git/branches",
            "/v1/git/status",
            "/v1/git/commits/{sha}",
            "/v1/git/commits/{sha}/diff",
            "/v1/git/graph",
        ] {
            assert_eq!(required(&Method::GET, pattern), Level::Read, "{pattern}");
            assert_eq!(
                refusal(&Grant::Level(Level::Read), &Method::GET, pattern),
                None,
                "{pattern}"
            );
        }
    }
}
