use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use poolparty::{
    config::StateDirectory, domain::*, live::LiveAccount, maintenance::CredentialMaintenance,
    managed::ManagedInventory, ports::*, runtime::Router, storage::SqliteLedger,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Mutex;

fn credential(name: &str) -> CredentialId {
    CredentialId::new(name).unwrap()
}
fn auth(account: &str, expiry: i64) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"sub":"synthetic-subject","exp":expiry,"https://api.openai.com/auth":{"chatgpt_account_id":account}})).unwrap());
    json!({"tokens":{"account_id":account,"access_token":format!("e30.{payload}.signature"),"refresh_token":"synthetic-refresh"}}).to_string()
}
struct FakeClock(AtomicI64);
impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        self.0.load(Ordering::SeqCst)
    }
}
struct FakeStore {
    entries: Mutex<BTreeMap<CredentialId, (u64, String)>>,
    fail_primary: AtomicBool,
    reads: AtomicUsize,
}
#[async_trait]
impl CredentialStore for FakeStore {
    async fn load(&self, reference: &CredentialRef) -> Result<SecretValue> {
        if self.fail_primary.load(Ordering::SeqCst) && reference.id.as_str() == "credential-a" {
            return Err(Error::new(
                ErrorCode::CredentialUnavailable,
                "synthetic unavailable",
            ));
        }
        let entries = self.entries.lock().await;
        let (generation, secret) = entries.get(&reference.id).unwrap();
        if generation != &reference.generation {
            return Err(Error::new(
                ErrorCode::CredentialUnavailable,
                "synthetic stale",
            ));
        }
        Ok(SecretValue::new(secret.clone()))
    }
    async fn replace(&self, expected: &CredentialRef, next: SecretValue) -> Result<CredentialRef> {
        let mut entries = self.entries.lock().await;
        let state = entries.get_mut(&expected.id).unwrap();
        if state.0 != expected.generation {
            return Err(Error::new(
                ErrorCode::CredentialUnavailable,
                "synthetic stale",
            ));
        }
        state.0 += 1;
        state.1 = next.expose().to_owned();
        Ok(CredentialRef {
            id: expected.id.clone(),
            generation: state.0,
        })
    }
}
#[async_trait]
impl VersionedCredentialStore for FakeStore {
    async fn latest(&self, id: &CredentialId) -> Result<(CredentialRef, SecretValue)> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_primary.load(Ordering::SeqCst) && id.as_str() == "credential-a" {
            return Err(Error::new(
                ErrorCode::CredentialUnavailable,
                "synthetic unavailable",
            ));
        }
        let entries = self.entries.lock().await;
        let (generation, secret) = entries.get(id).unwrap();
        Ok((
            CredentialRef {
                id: id.clone(),
                generation: *generation,
            },
            SecretValue::new(secret.clone()),
        ))
    }
}
struct Refresher(AtomicUsize);
#[async_trait]
impl CredentialRefresher for Refresher {
    async fn refresh(&self, secret: &SecretValue, now: Timestamp) -> Result<SecretValue> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let parsed: serde_json::Value = serde_json::from_str(secret.expose()).unwrap();
        Ok(SecretValue::new(auth(
            parsed["tokens"]["account_id"].as_str().unwrap(),
            now / 1000 + 3600,
        )))
    }
}
struct Collector {
    status: Mutex<std::result::Result<CapacityStatus, ErrorCode>>,
    calls: AtomicUsize,
}
#[async_trait]
impl UsageCollector for Collector {
    async fn collect(
        &self,
        account: &Account,
        _: &SecretValue,
        now: Timestamp,
    ) -> Result<UsageObservation> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let status = (*self.status.lock().await)
            .map_err(|code| Error::new(code, "synthetic collection failure"))?;
        Ok(UsageObservation {
            owner: account.quota_owner.clone(),
            observed_at: now,
            valid_until: now + 60_000,
            status,
            windows: vec![],
            balances: vec![],
            source: "synthetic".into(),
            provider_available: None,
        })
    }
}
fn enrollment(label: &str) -> LiveAccount {
    LiveAccount {
        id: AccountId::new(format!("account-{label}")).unwrap(),
        product: Product::CodexSubscription,
        quota_owner: QuotaOwnerId::new(format!("owner-{label}")).unwrap(),
        pool: PoolId::new("pool-a").unwrap(),
        credential: credential(&format!("credential-{label}")),
        model: "model-a".into(),
        expected_account_id: Some(format!("upstream-{label}")),
        max_concurrency: 2,
        unknown_capacity: UnknownCapacityPolicy::AllowUnderLocalCap,
        usage_url: None,
        inference_url: None,
        probe: None,
    }
}
struct Fixture {
    _directory: TempDir,
    state: Arc<StateDirectory>,
    ledger: Arc<SqliteLedger>,
    store: Arc<FakeStore>,
    refresher: Arc<Refresher>,
    collector: Arc<Collector>,
    clock: Arc<FakeClock>,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap().join("state");
        let state = Arc::new(StateDirectory::acquire(path).unwrap());
        let ledger = Arc::new(SqliteLedger::open(&state.database).unwrap());
        Self {
            _directory: directory,
            state,
            ledger,
            store: Arc::new(FakeStore {
                entries: Mutex::new(BTreeMap::from([
                    (credential("credential-a"), (7, auth("upstream-a", 7200))),
                    (credential("credential-b"), (4, auth("upstream-b", 7200))),
                ])),
                fail_primary: AtomicBool::new(false),
                reads: AtomicUsize::new(0),
            }),
            refresher: Arc::new(Refresher(AtomicUsize::new(0))),
            collector: Arc::new(Collector {
                status: Mutex::new(Ok(CapacityStatus::Available)),
                calls: AtomicUsize::new(0),
            }),
            clock: Arc::new(FakeClock(AtomicI64::new(1_000_000))),
        }
    }
    fn inventory(&self, accounts: Vec<LiveAccount>) -> Arc<ManagedInventory> {
        let ids = accounts
            .iter()
            .map(|account| account.credential.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let maintenance = CredentialMaintenance::new(
            self.store.clone(),
            self.refresher.clone(),
            self.state.clone(),
            self.ledger.clone(),
            ids,
        )
        .unwrap();
        Arc::new(
            ManagedInventory::new(
                accounts,
                maintenance,
                self.store.clone(),
                self.collector.clone(),
                self.ledger.clone(),
                self.clock.clone(),
            )
            .unwrap(),
        )
    }
    fn principal(&self) -> Principal {
        Principal {
            id: PrincipalId::new("caller-a").unwrap(),
            pools: BTreeSet::from([PoolId::new("pool-a").unwrap()]),
        }
    }
    async fn binding(&self, session: &str, account: &str) -> Result<Binding> {
        self.ledger
            .create_binding(
                &self.principal(),
                CreateBinding {
                    session: ClientSessionId::new(session).unwrap(),
                    pool: PoolId::new("pool-a").unwrap(),
                    product: Product::CodexSubscription,
                    model: "model-a".into(),
                    account: Some(AccountId::new(account).unwrap()),
                    effort: None,
                },
                self.clock.now(),
            )
            .await
    }
    async fn admit(&self, binding: &Binding) -> Result<PreparedAttempt> {
        self.ledger
            .admit(
                &self.principal(),
                Admission {
                    binding: binding.id.clone(),
                    operation: None,
                    request_fingerprint: "synthetic-fingerprint".into(),
                    model: "model-a".into(),
                    effort: None,
                },
                self.clock.now(),
            )
            .await
    }
    fn advance(&self, millis: i64) {
        self.clock.0.fetch_add(millis, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn credential_aliases_share_bounded_freshness_and_usage_collection() {
    let fixture = Fixture::new();
    let primary = enrollment("a");
    let mut alias = primary.clone();
    alias.id = AccountId::new("account-alias").unwrap();
    let inventory = fixture.inventory(vec![primary, alias]);
    inventory.sync_all().await.unwrap();
    assert_eq!(fixture.store.reads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.collector.calls.load(Ordering::SeqCst), 1);
    let binding = fixture.binding("session-a", "account-alias").await.unwrap();
    for _ in 0..4 {
        inventory.prepare(&binding).await.unwrap();
    }
    assert_eq!(fixture.store.reads.load(Ordering::SeqCst), 1);
    fixture.advance(30_001);
    inventory.prepare(&binding).await.unwrap();
    assert_eq!(fixture.store.reads.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.collector.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn due_refresh_is_persisted_before_admission_and_keeps_the_account_binding() {
    let fixture = Fixture::new();
    let inventory = fixture.inventory(vec![enrollment("a")]);
    inventory.sync_all().await.unwrap();
    let binding = fixture.binding("session-a", "account-a").await.unwrap();
    fixture.clock.0.store(7_000_000, Ordering::SeqCst);
    inventory.prepare(&binding).await.unwrap();
    let prepared = fixture.admit(&binding).await.unwrap();
    assert_eq!(prepared.binding, binding);
    assert_eq!(prepared.attempt.credential.generation, 8);
    assert_eq!(fixture.refresher.0.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .store
            .load(&prepared.attempt.credential)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn periodic_credential_failure_disables_persisted_account_and_syncs_healthy_accounts() {
    let fixture = Fixture::new();
    let inventory = fixture.inventory(vec![enrollment("a"), enrollment("b")]);
    inventory.sync_all().await.unwrap();
    let binding = fixture.binding("session-a", "account-a").await.unwrap();
    fixture.store.fail_primary.store(true, Ordering::SeqCst);
    fixture.advance(30_001);
    assert_eq!(
        inventory.sync_all().await.unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
    assert_eq!(
        inventory.prepare(&binding).await.unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
    assert_eq!(
        fixture.admit(&binding).await.unwrap_err().code,
        ErrorCode::NoEligibleAccount
    );
    assert_eq!(
        fixture
            .binding("fresh-a", "account-a")
            .await
            .unwrap_err()
            .code,
        ErrorCode::NoEligibleAccount
    );
    fixture.binding("fresh-b", "account-b").await.unwrap();
    fixture.store.fail_primary.store(false, Ordering::SeqCst);
    fixture.advance(5_001);
    inventory.prepare(&binding).await.unwrap();
    assert_eq!(fixture.admit(&binding).await.unwrap().binding, binding);
}

#[tokio::test]
async fn unknown_collection_after_restart_cannot_clear_exhaustion_or_auth_failure() {
    for failure in [
        CapacityStatus::Exhausted,
        CapacityStatus::ReauthenticationRequired,
    ] {
        let fixture = Fixture::new();
        let inventory = fixture.inventory(vec![enrollment("a")]);
        inventory.sync_all().await.unwrap();
        let binding = fixture.binding("session-a", "account-a").await.unwrap();
        *fixture.collector.status.lock().await = Ok(failure);
        fixture.advance(30_001);
        inventory.prepare(&binding).await.unwrap();
        drop(inventory);
        *fixture.collector.status.lock().await = Ok(CapacityStatus::Unknown);
        fixture.advance(60_001);
        let restarted = fixture.inventory(vec![enrollment("a")]);
        restarted.prepare(&binding).await.unwrap();
        let recorded = fixture
            .ledger
            .usage_observation(&QuotaOwnerId::new("owner-a").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recorded.status, failure);
        assert_eq!(
            fixture.admit(&binding).await.unwrap_err().code,
            if failure == CapacityStatus::Exhausted {
                ErrorCode::SessionQuotaExhausted
            } else {
                ErrorCode::ReauthenticationRequired
            }
        );
    }
}

#[tokio::test]
async fn collector_auth_errors_are_durable_and_transient_errors_follow_unknown_policy() {
    let fixture = Fixture::new();
    let inventory = fixture.inventory(vec![enrollment("a")]);
    inventory.sync_all().await.unwrap();
    let binding = fixture.binding("session-a", "account-a").await.unwrap();
    fixture.advance(60_001);
    *fixture.collector.status.lock().await = Err(ErrorCode::UpstreamUnavailable);
    inventory.prepare(&binding).await.unwrap();
    let accepted = fixture.admit(&binding).await.unwrap().attempt;
    fixture
        .ledger
        .settle(&accepted.id, Settlement::NotDispatched, fixture.clock.now())
        .await
        .unwrap();
    *fixture.collector.status.lock().await = Err(ErrorCode::ReauthenticationRequired);
    fixture.advance(5_001);
    inventory.prepare(&binding).await.unwrap();
    assert_eq!(
        fixture.admit(&binding).await.unwrap_err().code,
        ErrorCode::ReauthenticationRequired
    );
}

struct FailingPreparation(AtomicUsize);
#[async_trait]
impl RequestPreparation for FailingPreparation {
    async fn prepare(&self, _: &Binding) -> Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(Error::new(
            ErrorCode::CredentialUnavailable,
            "synthetic preparation failure",
        ))
    }
}
struct CountingTransport(AtomicUsize);
#[async_trait]
impl Transport for CountingTransport {
    async fn send(
        &self,
        _: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse, TransportError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(UpstreamResponse {
            status: 200,
            headers: vec![],
            stream: Box::pin(futures_util::stream::pending()),
        })
    }
}

#[tokio::test]
async fn router_prepares_only_valid_bound_requests_and_failure_creates_no_attempt() {
    let fixture = Fixture::new();
    fixture
        .inventory(vec![enrollment("a")])
        .sync_all()
        .await
        .unwrap();
    let binding = fixture.binding("session-a", "account-a").await.unwrap();
    let preparation = Arc::new(FailingPreparation(AtomicUsize::new(0)));
    let transport = Arc::new(CountingTransport(AtomicUsize::new(0)));
    let router = Router::new(
        fixture.ledger.clone(),
        transport.clone(),
        fixture.store.clone(),
        fixture.clock.clone(),
    )
    .with_preparation(preparation.clone());
    for model in ["wrong-model", "model-a"] {
        let error = router
            .execute(
                &fixture.principal(),
                binding.id.clone(),
                None,
                Protocol::Responses,
                Bytes::from(json!({"model":model,"stream":true}).to_string()),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.code,
            if model == "wrong-model" {
                ErrorCode::IntentConflict
            } else {
                ErrorCode::CredentialUnavailable
            }
        );
        assert_eq!(error.request_state, DispatchCertainty::NotDispatched);
    }
    assert_eq!(preparation.0.load(Ordering::SeqCst), 1);
    assert_eq!(transport.0.load(Ordering::SeqCst), 0);
    let connection = rusqlite::Connection::open(&fixture.state.database).unwrap();
    let attempts: u64 = connection
        .query_row("SELECT count(*) FROM attempts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(attempts, 0);
}

#[tokio::test]
async fn response_and_detached_settlement_retain_router_ownership() {
    let fixture = Fixture::new();
    fixture
        .inventory(vec![enrollment("a")])
        .sync_all()
        .await
        .unwrap();
    let binding = fixture.binding("session-a", "account-a").await.unwrap();
    let ownership = Arc::new(());
    let weak = Arc::downgrade(&ownership);
    let router = Router::new(
        fixture.ledger.clone(),
        Arc::new(CountingTransport(AtomicUsize::new(0))),
        fixture.store.clone(),
        fixture.clock.clone(),
    )
    .with_ownership(ownership.clone());
    drop(ownership);
    let response = router
        .execute(
            &fixture.principal(),
            binding.id,
            None,
            Protocol::Responses,
            Bytes::from_static(br#"{"model":"model-a","stream":true}"#),
        )
        .await
        .unwrap();
    let attempt = response.attempt_id.clone();
    drop(router);
    assert!(weak.upgrade().is_some());
    drop(response);
    tokio::time::timeout(Duration::from_secs(2), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture
            .ledger
            .attempt(&fixture.principal(), &attempt)
            .await
            .unwrap()
            .state,
        AttemptState::Uncertain
    );
}
