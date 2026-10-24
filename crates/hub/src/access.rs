//! Who may call what (docs/API-AUTH.md §3, docs/tasks/DOGFOOD.md §7): credentials have a
//! level, routes require one. The routes say nothing about credentials; A1 (users and
//! devices) replaces how a credential gets its level, not this table.

use axum::http::Method;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    Level(Level),
    /// An agent's session token: it only reaches `/v1/me*`.
    Agent,
}

/// The level `method` on the route `pattern` (as the router names it, e.g.
/// `/v1/accounts/{id}`) requires: reading is read; changing is write, except what touches
/// credentials or the hub's life, which is admin.
pub fn required(method: &Method, pattern: &str) -> Level {
    let admin = matches!(
        (method.as_str(), pattern),
        ("POST", "/v1/shutdown")
            | ("POST", "/v1/accounts")
            | ("DELETE", "/v1/accounts/{id}")
            | ("POST", "/v1/accounts/{id}/login")
    );
    if admin {
        Level::Admin
    } else if *method == Method::GET {
        Level::Read
    } else {
        Level::Write
    }
}

/// `None` when `grant` may call the route, else why not (for a 403).
pub fn refusal(grant: Grant, method: &Method, pattern: &str) -> Option<String> {
    let needed = required(method, pattern);
    match grant {
        Grant::Agent => Some(format!(
            "an agent's session token reaches /v1/me only; {method} {pattern} needs the {} level",
            needed.word()
        )),
        Grant::Level(level) if level < needed => Some(format!(
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
        assert_eq!(required(&Method::DELETE, "/v1/accounts/{id}"), Level::Admin);
    }

    #[test]
    fn refusals_name_the_missing_level() {
        let read = Grant::Level(Level::Read);
        assert_eq!(refusal(read, &Method::GET, "/v1/runs"), None);
        let why = refusal(read, &Method::POST, "/v1/runs").unwrap();
        assert!(
            why.contains("read level") && why.contains("needs write"),
            "{why}"
        );
        assert!(
            refusal(Grant::Level(Level::Write), &Method::POST, "/v1/shutdown")
                .unwrap()
                .contains("needs admin")
        );
        assert_eq!(
            refusal(Grant::Level(Level::Admin), &Method::POST, "/v1/shutdown"),
            None
        );
        assert!(refusal(Grant::Agent, &Method::GET, "/v1/agents").is_some());
    }
}
