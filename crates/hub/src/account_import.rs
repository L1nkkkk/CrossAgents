//! Importing a login that is already on this machine into an account of its own
//! (docs/research/magpie-lancer.md §2.11): `~/.codex/auth.json` and
//! `~/.claude/.credentials.json` are copied into `<crossagents home>/accounts/<id>/`, so the
//! account has an independent home while the original tool keeps its own file untouched.
//!
//! A copied token is still the same token: when the new account refreshes it, the vendor may
//! sign the original tool out. The API therefore offers the warning [`ImportableAccount`]
//! carries and refuses to import unless the request confirms it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use crossagents_api_types::{
    CreateAccountRequest, ImportAccountRequest, ImportAccountsResponse, ImportableAccount,
};
use crossagents_core::{Account, CoreError};

use crate::accounts::Accounts;
use crate::api::{ApiResult, AppState};

/// One vendor login the hub knows how to copy.
struct Source {
    provider: &'static str,
    /// Directory under the user's home.
    directory: &'static str,
    /// The file inside it that holds the login.
    file: &'static str,
    /// What copying it means for the tool it came from.
    warning: &'static str,
}

const WARNING_CODEX: &str = "导入的是同一份 ChatGPT 登录令牌。之后这个账号刷新令牌时，原来的 Codex 会被登出，需要重新登录。";
const WARNING_CLAUDE: &str =
    "导入的是同一份 Claude 登录凭据。之后两个目录都可能刷新令牌，刷新后另一边可能需要重新登录。";

const SOURCES: [Source; 2] = [
    Source {
        provider: "codex",
        directory: ".codex",
        file: "auth.json",
        warning: WARNING_CODEX,
    },
    Source {
        provider: "claude",
        directory: ".claude",
        file: ".credentials.json",
        warning: WARNING_CLAUDE,
    },
];

/// `GET /v1/accounts/import`: the logins this machine has, with what importing each means.
pub async fn import_candidates(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<ImportAccountsResponse>> {
    Ok(Json(ImportAccountsResponse {
        candidates: candidates(&user_home(&state)?),
    }))
}

/// `POST /v1/accounts/import`: copies one login into a new account's own home.
pub async fn import_account(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ImportAccountRequest>,
) -> ApiResult<(StatusCode, Json<Account>)> {
    let account = import(&state.accounts, &user_home(&state)?, req).await?;
    Ok((StatusCode::CREATED, Json(account)))
}

/// The user's home directory the vendor logins live under.
fn user_home(state: &AppState) -> Result<PathBuf, CoreError> {
    dirs::home_dir().ok_or_else(|| {
        CoreError::Storage(format!(
            "cannot determine the user's home directory (crossagents home is {})",
            state.home.display()
        ))
    })
}

/// The logins found under `user_home`; nothing here reads a file.
pub fn candidates(user_home: &Path) -> Vec<ImportableAccount> {
    SOURCES
        .iter()
        .filter_map(|source| {
            let file = source_path(user_home, source);
            file.is_file().then(|| ImportableAccount {
                provider: source.provider.to_owned(),
                source: file.display().to_string(),
                warning: source.warning.to_owned(),
            })
        })
        .collect()
}

/// Copies `req.provider`'s login under `user_home` into a fresh account and returns it.
/// The source file is only read. A request that has not confirmed the token warning is
/// refused before anything is created.
pub async fn import(
    accounts: &Accounts,
    user_home: &Path,
    req: ImportAccountRequest,
) -> Result<Account, CoreError> {
    let source = SOURCES
        .iter()
        .find(|source| source.provider == req.provider)
        .ok_or_else(|| {
            CoreError::Conflict(format!(
                "there is no machine login to import for {:?} (only codex and claude)",
                req.provider
            ))
        })?;
    if !req.confirm {
        return Err(CoreError::Conflict(
            "importing copies a login token: refreshing it can sign the original tool out, so the import must be confirmed"
                .into(),
        ));
    }
    let from = source_path(user_home, source);
    let bytes = std::fs::read(&from).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CoreError::NotFound {
                kind: "login file",
                id: from.display().to_string(),
            }
        } else {
            CoreError::Storage(format!("cannot read {}: {err}", from.display()))
        }
    })?;
    check_login(&bytes, source)?;

    let account = accounts
        .create(CreateAccountRequest {
            codebuddy_site: None,
            id: req.id,
            provider: req.provider,
            label: req.label,
            home: None,
            import_default: false,
        })
        .await?;
    let to = account.home.join(source.file);
    if let Err(err) = write_secret(&to, &bytes) {
        // The account was registered a moment ago; leave no half-imported record behind.
        let _ = accounts.remove(&account.id).await;
        return Err(err);
    }
    Ok(account)
}

fn source_path(user_home: &Path, source: &Source) -> PathBuf {
    user_home.join(source.directory).join(source.file)
}

/// Refuses a file that is not a login: an empty or unrelated JSON file would only create an
/// account that reports itself logged out.
fn check_login(bytes: &[u8], source: &Source) -> Result<(), CoreError> {
    let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| {
        CoreError::Conflict(format!("{} is not a JSON login file: {err}", source.file))
    })?;
    if !json.is_object() {
        return Err(CoreError::Conflict(format!(
            "{} does not hold a login",
            source.file
        )));
    }
    if source.provider == "codex" {
        let token = json["tokens"]["access_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty());
        let api_key = json["OPENAI_API_KEY"]
            .as_str()
            .is_some_and(|key| !key.is_empty());
        if !token && !api_key {
            return Err(CoreError::Conflict(format!(
                "{} holds no ChatGPT or API-key login",
                source.file
            )));
        }
    }
    Ok(())
}

/// Writes a copy that only the user can read.
fn write_secret(to: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    std::fs::write(to, bytes)
        .map_err(|err| CoreError::Storage(format!("cannot write {}: {err}", to.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| CoreError::Storage(format!("cannot protect {}: {err}", to.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crossagents_core::{
        AccountProvider, AccountStatus, EventBus, LoginCommand, ModelInfo, SessionUsage, Store,
    };
    use crossagents_store_sqlite::SqliteStore;

    use super::*;

    /// A provider whose accounts need nothing but a home: enough for [`Accounts::create`].
    struct Stub;

    #[async_trait::async_trait]
    impl AccountProvider for Stub {
        fn name(&self) -> &str {
            "codex"
        }
        fn default_home(&self) -> Option<PathBuf> {
            None
        }
        async fn status(&self, _account: &Account) -> AccountStatus {
            AccountStatus {
                logged_in: false,
                auth_mode: None,
                email: None,
                plan: None,
                subscription_until: None,
                rate_limits: Vec::new(),
                limit_reached: false,
                credits: None,
                limits: Vec::new(),
                reset_credits: Vec::new(),
                source: None,
                as_of: None,
                note: None,
            }
        }
        async fn sessions(&self, _account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
            Ok(Vec::new())
        }
        async fn models(&self, _account: &Account) -> Vec<ModelInfo> {
            Vec::new()
        }
        fn login_command(&self, _account: &Account) -> LoginCommand {
            LoginCommand {
                command: vec!["true".into()],
                env: BTreeMap::new(),
                env_remove: Vec::new(),
                serves: None,
            }
        }
        fn agent_env(&self, _account: &Account) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    /// A stub under another provider's name, so whichever login is imported has a provider.
    struct Other(&'static str);

    #[async_trait::async_trait]
    impl AccountProvider for Other {
        fn name(&self) -> &str {
            self.0
        }
        fn default_home(&self) -> Option<PathBuf> {
            None
        }
        async fn status(&self, _account: &Account) -> AccountStatus {
            Stub.status(_account).await
        }
        async fn sessions(&self, _account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
            Ok(Vec::new())
        }
        async fn models(&self, _account: &Account) -> Vec<ModelInfo> {
            Vec::new()
        }
        fn login_command(&self, _account: &Account) -> LoginCommand {
            Stub.login_command(_account)
        }
        fn agent_env(&self, _account: &Account) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    /// A temp user home and hub, with the logins `write` puts there.
    async fn setup(
        write: impl FnOnce(&Path),
    ) -> (tempfile::TempDir, PathBuf, Arc<Accounts>, Arc<dyn Store>) {
        let dir = tempfile::tempdir().unwrap();
        let user_home = dir.path().join("user");
        std::fs::create_dir_all(&user_home).unwrap();
        write(&user_home);
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let bus = Arc::new(EventBus::new(store.clone()));
        let accounts = Arc::new(Accounts::new(
            bus,
            vec![Arc::new(Stub), Arc::new(Other("claude"))],
            dir.path().join("crossagents/accounts"),
        ));
        (dir, user_home, accounts, store)
    }

    fn codex_auth(user_home: &Path) {
        std::fs::create_dir_all(user_home.join(".codex")).unwrap();
        std::fs::write(
            user_home.join(".codex/auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"secret-access","refresh_token":"secret-refresh"}}"#,
        )
        .unwrap();
    }

    #[test]
    fn offers_only_the_logins_this_machine_has() {
        let dir = tempfile::tempdir().unwrap();
        let user_home = dir.path();

        assert!(candidates(user_home).is_empty());

        codex_auth(user_home);
        let found = candidates(user_home);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].provider, "codex");
        assert!(found[0].source.ends_with("auth.json"));
        assert!(found[0].warning.contains("刷新令牌"));

        std::fs::create_dir_all(user_home.join(".claude")).unwrap();
        std::fs::write(
            user_home.join(".claude/.credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"secret"}}"#,
        )
        .unwrap();
        let providers: Vec<_> = candidates(user_home)
            .into_iter()
            .map(|candidate| candidate.provider)
            .collect();
        assert_eq!(providers, ["codex", "claude"]);
    }

    #[tokio::test]
    async fn copies_the_login_into_an_independent_home_and_keeps_the_original() {
        let (dir, user_home, accounts, store) = setup(codex_auth).await;
        let original = user_home.join(".codex/auth.json");
        let before = std::fs::read(&original).unwrap();

        let account = import(
            &accounts,
            &user_home,
            ImportAccountRequest {
                provider: "codex".into(),
                id: "codex-import".into(),
                label: Some("本机 ChatGPT".into()),
                confirm: true,
            },
        )
        .await
        .unwrap();

        // A home of its own under the hub, filled with a byte-for-byte copy.
        assert_eq!(account.id, "codex-import");
        assert_eq!(account.label, "本机 ChatGPT");
        assert!(
            account
                .home
                .starts_with(dir.path().join("crossagents/accounts"))
        );
        let copied = account.home.join("auth.json");
        assert_eq!(std::fs::read(&copied).unwrap(), before);
        assert_eq!(std::fs::read(&original).unwrap(), before);
        assert_eq!(
            store.list_accounts().await.unwrap().len(),
            1,
            "the new account is registered"
        );

        // The copy is the login the provider would read for this home.
        assert!(copied.is_file());
    }

    #[tokio::test]
    async fn imports_claudes_credentials_under_their_own_name() {
        let (dir, user_home, accounts, _store) = setup(|home| {
            std::fs::create_dir_all(home.join(".claude")).unwrap();
            std::fs::write(
                home.join(".claude/.credentials.json"),
                r#"{"claudeAiOauth":{"accessToken":"secret"}}"#,
            )
            .unwrap();
        })
        .await;

        let account = import(
            &accounts,
            &user_home,
            ImportAccountRequest {
                provider: "claude".into(),
                id: "claude-import".into(),
                label: None,
                confirm: true,
            },
        )
        .await
        .unwrap();
        assert!(
            account
                .home
                .starts_with(dir.path().join("crossagents/accounts"))
        );
        assert!(account.home.join(".credentials.json").is_file());
    }

    #[tokio::test]
    async fn will_not_import_without_confirmation() {
        let (_dir, user_home, accounts, store) = setup(codex_auth).await;
        let err = import(
            &accounts,
            &user_home,
            ImportAccountRequest {
                provider: "codex".into(),
                id: "codex-import".into(),
                label: None,
                confirm: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CoreError::Conflict(_)));
        assert!(store.list_accounts().await.unwrap().is_empty());
        assert!(
            !user_home
                .join("../crossagents/accounts/codex-import")
                .exists()
        );
    }

    #[tokio::test]
    async fn a_missing_login_is_reported_and_nothing_is_created() {
        let (_dir, user_home, accounts, store) = setup(|_| {}).await;
        let err = import(
            &accounts,
            &user_home,
            ImportAccountRequest {
                provider: "codex".into(),
                id: "codex-import".into(),
                label: None,
                confirm: true,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CoreError::NotFound { .. }));
        assert!(store.list_accounts().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_login_is_refused() {
        let (_dir, user_home, accounts, store) = setup(|home| {
            std::fs::create_dir_all(home.join(".codex")).unwrap();
            std::fs::write(home.join(".codex/auth.json"), r#"{"auth_mode":"chatgpt"}"#).unwrap();
        })
        .await;
        let err = import(
            &accounts,
            &user_home,
            ImportAccountRequest {
                provider: "codex".into(),
                id: "codex-import".into(),
                label: None,
                confirm: true,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CoreError::Conflict(_)));
        assert!(store.list_accounts().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_provider_without_a_machine_login_is_refused() {
        let (_dir, user_home, accounts, _store) = setup(codex_auth).await;
        let err = import(
            &accounts,
            &user_home,
            ImportAccountRequest {
                provider: "cursor".into(),
                id: "cursor-import".into(),
                label: None,
                confirm: true,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CoreError::Conflict(_)));
    }
}
