use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use poolparty::{
    config::StateDirectory, domain::*, maintenance::CredentialMaintenance, ports::*,
    storage::SqliteLedger,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::{Mutex, Notify};

const NOW: Timestamp = 1_000_000;

fn credential() -> CredentialId {
    CredentialId::new("credential-a").unwrap()
}
fn reference(generation: u64) -> CredentialRef {
    CredentialRef {
        id: credential(),
        generation,
    }
}
fn bundle(account: &str, subject: &str, expiry: u64, refresh: &str) -> String {
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "exp":expiry, "sub":subject,
            "https://api.openai.com/auth":{"chatgpt_account_id":account}
        }))
        .unwrap(),
    );
    json!({"tokens":{
        "account_id":account, "access_token":format!("e30.{payload}.synthetic-signature"),
        "refresh_token":refresh
    }})
    .to_string()
}
fn stale_bundle() -> String {
    bundle("account-a", "subject-a", 100, "synthetic-old-refresh")
}
fn fresh_bundle() -> String {
    bundle("account-a", "subject-a", 7200, "synthetic-new-refresh")
}
fn store_error() -> Error {
    Error::new(ErrorCode::CredentialUnavailable, "synthetic store failure")
}

#[derive(Clone, Copy)]
enum StoreFailure {
    None,
    BeforeWrite,
    AfterWrite,
}
struct FakeStore {
    state: Mutex<(u64, String)>,
    failure: StoreFailure,
    replacements: AtomicUsize,
}
#[async_trait]
impl CredentialStore for FakeStore {
    async fn load(&self, expected: &CredentialRef) -> Result<SecretValue> {
        let state = self.state.lock().await;
        if expected.id != credential() || expected.generation != state.0 {
            return Err(store_error());
        }
        Ok(SecretValue::new(state.1.clone()))
    }
    async fn replace(&self, expected: &CredentialRef, next: SecretValue) -> Result<CredentialRef> {
        self.replacements.fetch_add(1, Ordering::SeqCst);
        let mut state = self.state.lock().await;
        if expected.id != credential() || expected.generation != state.0 {
            return Err(store_error());
        }
        if matches!(self.failure, StoreFailure::BeforeWrite) {
            return Err(store_error());
        }
        state.0 += 1;
        state.1 = next.expose().to_owned();
        if matches!(self.failure, StoreFailure::AfterWrite) {
            return Err(store_error());
        }
        Ok(reference(state.0))
    }
}
#[async_trait]
impl VersionedCredentialStore for FakeStore {
    async fn latest(&self, id: &CredentialId) -> Result<(CredentialRef, SecretValue)> {
        if id != &credential() {
            return Err(store_error());
        }
        let state = self.state.lock().await;
        Ok((reference(state.0), SecretValue::new(state.1.clone())))
    }
}

struct FakeRefresher {
    response: Option<String>,
    calls: AtomicUsize,
    blocked: bool,
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl CredentialRefresher for FakeRefresher {
    async fn refresh(&self, _: &SecretValue, _: Timestamp) -> Result<SecretValue> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.blocked {
            self.release.notified().await;
        }
        self.response
            .as_ref()
            .map(|value| SecretValue::new(value.clone()))
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::ReauthenticationRequired,
                    "synthetic exchange failure",
                )
            })
    }
}

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
    state: Arc<StateDirectory>,
    ledger: Arc<SqliteLedger>,
    store: Arc<FakeStore>,
    refresher: Arc<FakeRefresher>,
}
impl Fixture {
    fn new(failure: StoreFailure, response: Option<String>, blocked: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap().join("state");
        let state = Arc::new(StateDirectory::acquire(&path).unwrap());
        let ledger = Arc::new(SqliteLedger::open(&state.database).unwrap());
        Self {
            _directory: directory,
            path,
            state,
            ledger,
            store: Arc::new(FakeStore {
                state: Mutex::new((7, stale_bundle())),
                failure,
                replacements: AtomicUsize::new(0),
            }),
            refresher: Arc::new(FakeRefresher {
                response,
                calls: AtomicUsize::new(0),
                blocked,
                entered: Notify::new(),
                release: Notify::new(),
            }),
        }
    }
    fn maintenance(&self) -> CredentialMaintenance {
        CredentialMaintenance::new(
            self.store.clone(),
            self.refresher.clone(),
            self.state.clone(),
            self.ledger.clone(),
            vec![credential()],
        )
        .unwrap()
    }
    fn marker(&self) -> PathBuf {
        self.path.join("refresh-credential-a.pending.json")
    }
    fn exchanges(&self) -> usize {
        self.refresher.calls.load(Ordering::SeqCst)
    }
}
async fn resolve(manager: &CredentialMaintenance, force: bool) -> Result<CredentialRef> {
    manager
        .resolve(
            &credential(),
            Product::CodexSubscription,
            Some("account-a"),
            NOW,
            force,
        )
        .await
}

#[tokio::test]
async fn concurrent_nonforced_resolutions_refresh_once_and_publish_persisted_generation() {
    let fixture = Fixture::new(StoreFailure::None, Some(fresh_bundle()), false);
    let manager = fixture.maintenance();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let manager = manager.clone();
        tasks.push(tokio::spawn(async move { resolve(&manager, false).await }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap(), reference(8));
    }
    assert_eq!(fixture.exchanges(), 1);
    assert_eq!(fixture.store.replacements.load(Ordering::SeqCst), 1);
    assert!(!fixture.marker().exists());
    assert_eq!(
        fixture.store.load(&reference(8)).await.unwrap().expose(),
        fresh_bundle()
    );
    assert_eq!(
        fixture
            .ledger
            .advance_credential_generation(&reference(7))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn exchange_failure_leaves_a_durable_secret_free_marker_and_restart_fence() {
    let fixture = Fixture::new(StoreFailure::None, None, false);
    let manager = fixture.maintenance();
    assert_eq!(
        resolve(&manager, false).await.unwrap_err().code,
        ErrorCode::ReauthenticationRequired
    );
    assert!(fixture.marker().exists());
    let marker = std::fs::read_to_string(fixture.marker()).unwrap();
    assert!(!marker.contains("synthetic-old-refresh"));
    assert!(!marker.contains("synthetic-signature"));
    assert!(!marker.contains("subject-a"));
    assert!(!marker.contains("account-a"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(fixture.marker())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(manager);
    let Fixture {
        _directory,
        path,
        state,
        ledger,
        store,
        refresher,
    } = fixture;
    drop(state);
    drop(ledger);
    let state = Arc::new(StateDirectory::acquire(&path).unwrap());
    let ledger = Arc::new(SqliteLedger::open(&state.database).unwrap());
    let restarted =
        CredentialMaintenance::new(store, refresher.clone(), state, ledger, vec![credential()])
            .unwrap();
    assert_eq!(
        resolve(&restarted, true).await.unwrap_err().code,
        ErrorCode::ReauthenticationRequired
    );
    assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_writeback_stays_fenced_even_if_new_tokens_reach_the_store() {
    for failure in [StoreFailure::BeforeWrite, StoreFailure::AfterWrite] {
        let fixture = Fixture::new(failure, Some(fresh_bundle()), false);
        let manager = fixture.maintenance();
        assert_eq!(
            resolve(&manager, false).await.unwrap_err().code,
            ErrorCode::CredentialUnavailable
        );
        assert!(fixture.marker().exists());
        let (stored, _) = fixture.store.latest(&credential()).await.unwrap();
        assert_eq!(
            stored.generation,
            if matches!(failure, StoreFailure::AfterWrite) {
                8
            } else {
                7
            }
        );
        for force in [false, true] {
            assert_eq!(
                resolve(&manager, force).await.unwrap_err().code,
                ErrorCode::ReauthenticationRequired
            );
        }
        // New manager instance also cannot waive a failed preservation/readback check.
        assert_eq!(
            resolve(&fixture.maintenance(), false)
                .await
                .unwrap_err()
                .code,
            ErrorCode::ReauthenticationRequired
        );
        assert_eq!(fixture.exchanges(), 1);
        assert_eq!(fixture.store.replacements.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn stale_refresh_response_is_saved_to_preserve_rotated_token_but_remains_fenced() {
    let stale_rotated = bundle("account-a", "subject-a", 900, "synthetic-rotated-refresh");
    let fixture = Fixture::new(StoreFailure::None, Some(stale_rotated.clone()), false);
    let manager = fixture.maintenance();
    assert_eq!(
        resolve(&manager, false).await.unwrap_err().code,
        ErrorCode::ReauthenticationRequired
    );
    assert_eq!(
        fixture.store.load(&reference(8)).await.unwrap().expose(),
        stale_rotated
    );
    assert!(fixture.marker().exists());
    assert_eq!(
        resolve(&manager, true).await.unwrap_err().code,
        ErrorCode::ReauthenticationRequired
    );
    assert_eq!(fixture.exchanges(), 1);
}

#[tokio::test]
async fn refreshed_account_or_subject_mismatch_never_overwrites_current_bundle() {
    for response in [
        bundle("account-b", "subject-a", 7200, "new-refresh"),
        bundle("account-a", "subject-b", 7200, "new-refresh"),
    ] {
        let fixture = Fixture::new(StoreFailure::None, Some(response), false);
        assert_eq!(
            resolve(&fixture.maintenance(), false)
                .await
                .unwrap_err()
                .code,
            ErrorCode::ReauthenticationRequired
        );
        assert_eq!(fixture.store.replacements.load(Ordering::SeqCst), 0);
        assert_eq!(
            fixture.store.load(&reference(7)).await.unwrap().expose(),
            stale_bundle()
        );
        assert!(fixture.marker().exists());
    }
}

#[tokio::test]
async fn wrong_or_missing_enrolled_identity_prevents_exchange_and_marker_creation() {
    let fixture = Fixture::new(StoreFailure::None, Some(fresh_bundle()), false);
    let manager = fixture.maintenance();
    for expected in [Some("account-b"), None] {
        assert_eq!(
            manager
                .resolve(
                    &credential(),
                    Product::CodexSubscription,
                    expected,
                    NOW,
                    true
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::ReauthenticationRequired
        );
    }
    assert_eq!(fixture.exchanges(), 0);
    assert!(!fixture.marker().exists());
}

#[tokio::test]
async fn durable_generation_rollback_is_refused_before_exchange() {
    let fixture = Fixture::new(StoreFailure::None, Some(fresh_bundle()), false);
    fixture
        .ledger
        .advance_credential_generation(&reference(8))
        .await
        .unwrap();
    let reopened = Arc::new(SqliteLedger::open(&fixture.state.database).unwrap());
    let manager = CredentialMaintenance::new(
        fixture.store.clone(),
        fixture.refresher.clone(),
        fixture.state.clone(),
        reopened,
        vec![credential()],
    )
    .unwrap();
    assert_eq!(
        resolve(&manager, true).await.unwrap_err().code,
        ErrorCode::InvalidInput
    );
    assert_eq!(fixture.exchanges(), 0);
    assert!(!fixture.marker().exists());
}

#[tokio::test]
async fn caller_cancellation_does_not_abandon_owned_refresh_or_allow_duplicate_issuance() {
    let fixture = Fixture::new(StoreFailure::None, Some(fresh_bundle()), true);
    let manager = fixture.maintenance();
    let caller_manager = manager.clone();
    let caller = tokio::spawn(async move { resolve(&caller_manager, false).await });
    tokio::time::timeout(Duration::from_secs(2), fixture.refresher.entered.notified())
        .await
        .unwrap();
    assert!(fixture.marker().exists());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    fixture.refresher.release.notify_one();
    let next = tokio::time::timeout(Duration::from_secs(2), resolve(&manager, false))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next, reference(8));
    assert_eq!(fixture.exchanges(), 1);
    assert_eq!(fixture.store.replacements.load(Ordering::SeqCst), 1);
    assert!(!fixture.marker().exists());
}
