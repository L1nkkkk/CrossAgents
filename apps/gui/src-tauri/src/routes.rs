use std::{sync::LazyLock, time::Duration};

use regex::Regex;
use reqwest::Method;
use serde::Deserialize;

#[derive(Deserialize)]
struct Route {
    method: String,
    path: String,
}

// Desktop Hub API route allowlist. Streaming routes have their own commands.
static ROUTES: LazyLock<Vec<(String, Regex)>> = LazyLock::new(|| {
    let routes: Vec<Route> = serde_json::from_str(include_str!("../../shared/hub-routes.json"))
        .expect("valid desktop Hub route table");
    routes
        .into_iter()
        .map(|route| {
            let pattern = Regex::new(&route.path).expect("valid desktop Hub route pattern");
            (route.method, pattern)
        })
        .collect()
});

pub fn allowed(method: &Method, path: &str) -> bool {
    ROUTES
        .iter()
        .any(|(verb, pattern)| verb == method.as_str() && pattern.is_match(path))
}

pub fn timeout(method: &Method, path: &str) -> Duration {
    static COMPACT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("^/v1/agents/[^/]+/compact$").expect("valid compact route"));
    Duration::from_secs(if method == Method::POST && COMPACT.is_match(path) {
        15 * 60
    } else {
        30
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_functions_keep_the_existing_permissions() {
        assert!(allowed(&Method::POST, "/v1/agents"));
        assert!(allowed(&Method::DELETE, "/v1/agents/one/messages/two"));
        assert!(allowed(
            &Method::POST,
            "/v1/runs/one/tasks/two/files/resolve"
        ));
        assert!(!allowed(&Method::POST, "/v1/shutdown"));
        assert!(!allowed(&Method::GET, "/v1/events"));
        assert!(!allowed(&Method::GET, "/v1/terminals/one/output"));
        assert!(!allowed(&Method::GET, "/v1/agents/one/token"));
    }
}
