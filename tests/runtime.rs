use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use poolparty::{
    config::MemoryCredentials, domain::*, ports::*, providers::SyntheticTransport, runtime::Router,
    storage::SqliteLedger,
};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct TestClock;
impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        1000
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    ledger: Arc<SqliteLedger>,
    principal: Principal,
    credentials: Arc<MemoryCredentials>,
    binding: Binding,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap().join("ledger.sqlite3");
    let ledger = Arc::new(SqliteLedger::open(&path).unwrap());
    let pool = PoolId::new("pool-a").unwrap();
    let owner = QuotaOwnerId::new("owner-a").unwrap();
    let reference = CredentialRef {
        id: CredentialId::new("credential-a").unwrap(),
        generation: 1,
    };
    ledger
        .put_account(Account {
            id: AccountId::new("account-a").unwrap(),
            product: Product::CodexSubscription,
            quota_owner: owner.clone(),
            pools: BTreeSet::from([pool.clone()]),
            credential: reference.clone(),
            models: BTreeSet::from(["synthetic-model".into()]),
            enabled: true,
        })
        .await
        .unwrap();
    ledger
        .put_quota_policy(QuotaPolicy {
            owner: owner.clone(),
            max_concurrency: 1,
            unknown: UnknownCapacityPolicy::Reject,
        })
        .await
        .unwrap();
    ledger
        .observe(UsageObservation {
            owner,
            observed_at: 0,
            valid_until: 10000,
            status: CapacityStatus::Available,
            windows: vec![],
            balances: vec![],
            source: "fixture".into(),
            provider_available: None,
        })
        .await
        .unwrap();
    let principal = Principal {
        id: PrincipalId::new("caller-a").unwrap(),
        pools: BTreeSet::from([pool.clone()]),
    };
    let binding = ledger
        .create_binding(
            &principal,
            CreateBinding {
                session: ClientSessionId::new("session-a").unwrap(),
                pool,
                product: Product::CodexSubscription,
                model: "synthetic-model".into(),
                account: None,
                effort: None,
            },
            1000,
        )
        .await
        .unwrap();
    let credentials = Arc::new(
        MemoryCredentials::new(vec![(reference, SecretValue::new("fixture-secret".into()))])
            .unwrap(),
    );
    Fixture {
        _dir: dir,
        path,
        ledger,
        principal,
        credentials,
        binding,
    }
}

fn request() -> Bytes {
    Bytes::from_static(br#"{"model":"synthetic-model","input":"fixture","stream":true}"#)
}
fn router(f: &Fixture, transport: Arc<dyn Transport>) -> Router {
    Router::new(
        f.ledger.clone(),
        transport,
        f.credentials.clone(),
        Arc::new(TestClock),
    )
}

#[tokio::test]
async fn stream_settle_restart_resume_keeps_account_and_prevents_operation_replay() {
    let f = fixture().await;
    let service = router(&f, Arc::new(SyntheticTransport::new()));
    let op = OperationId::new("operation-a").unwrap();
    let mut response = service
        .execute(
            &f.principal,
            f.binding.id.clone(),
            Some(op.clone()),
            Protocol::Responses,
            request(),
        )
        .await
        .unwrap();
    let attempt = response.attempt_id.clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.stream.next().await {
        bytes.extend(chunk.unwrap());
    }
    assert!(
        String::from_utf8(bytes)
            .unwrap()
            .contains("response.completed")
    );
    assert_eq!(
        f.ledger
            .attempt(&f.principal, &attempt)
            .await
            .unwrap()
            .state,
        AttemptState::Succeeded
    );
    assert_eq!(
        service
            .execute(
                &f.principal,
                f.binding.id.clone(),
                Some(op),
                Protocol::Responses,
                request()
            )
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::OperationAlreadyExists
    );
    drop(response);
    drop(service);
    // A new SQLite connection represents the next service instance; no active request remains.
    let restarted = Arc::new(SqliteLedger::open(&f.path).unwrap());
    restarted.recover(2000).await.unwrap();
    let resumed = restarted
        .binding(&f.principal, &f.binding.id)
        .await
        .unwrap();
    assert_eq!(resumed.account, f.binding.account);
    let service = Router::new(
        restarted.clone(),
        Arc::new(SyntheticTransport::new()),
        f.credentials.clone(),
        Arc::new(TestClock),
    );
    let mut second = service
        .execute(
            &f.principal,
            resumed.id,
            Some(OperationId::new("operation-b").unwrap()),
            Protocol::Responses,
            request(),
        )
        .await
        .unwrap();
    while let Some(chunk) = second.stream.next().await {
        chunk.unwrap();
    }
    assert_eq!(
        restarted
            .attempt(&f.principal, &second.attempt_id)
            .await
            .unwrap()
            .state,
        AttemptState::Succeeded
    );
}

struct Truncated {
    calls: AtomicUsize,
}
#[async_trait]
impl Transport for Truncated {
    async fn send(
        &self,
        _: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse, TransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(UpstreamResponse {
            status: 200,
            headers: vec![],
            stream: Box::pin(futures_util::stream::iter([Ok(StreamEvent::Data(
                Bytes::from_static(b"event: response.created\n\n"),
            ))])),
        })
    }
}

#[tokio::test]
async fn partial_eof_fences_binding_and_retains_capacity_without_replay() {
    let f = fixture().await;
    let transport = Arc::new(Truncated {
        calls: AtomicUsize::new(0),
    });
    let service = router(&f, transport.clone());
    let mut response = service
        .execute(
            &f.principal,
            f.binding.id.clone(),
            None,
            Protocol::Responses,
            request(),
        )
        .await
        .unwrap();
    assert!(response.stream.next().await.unwrap().is_ok());
    assert_eq!(
        response
            .stream
            .next()
            .await
            .unwrap()
            .unwrap_err()
            .request_state,
        DispatchCertainty::Unknown
    );
    assert_eq!(
        f.ledger
            .attempt(&f.principal, &response.attempt_id)
            .await
            .unwrap()
            .state,
        AttemptState::Uncertain
    );
    assert_eq!(
        service
            .execute(
                &f.principal,
                f.binding.id.clone(),
                None,
                Protocol::Responses,
                request()
            )
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::SessionUncertain
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_an_unpolled_response_still_fences_the_attempt() {
    let f = fixture().await;
    let service = router(&f, Arc::new(SyntheticTransport::new()));
    let response = service
        .execute(
            &f.principal,
            f.binding.id.clone(),
            None,
            Protocol::Responses,
            request(),
        )
        .await
        .unwrap();
    let id = response.attempt_id.clone();
    drop(response);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if f.ledger.attempt(&f.principal, &id).await.unwrap().state == AttemptState::Uncertain {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn remote_continuation_is_rejected_before_any_dispatch() {
    let f = fixture().await;
    let transport = Arc::new(Truncated {
        calls: AtomicUsize::new(0),
    });
    let service = router(&f, transport.clone());
    let body = Bytes::from_static(
        br#"{"model":"synthetic-model","stream":true,"previous_response_id":"foreign-response"}"#,
    );
    let error = service
        .execute(
            &f.principal,
            f.binding.id.clone(),
            None,
            Protocol::Responses,
            body,
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(error.request_state, DispatchCertainty::NotDispatched);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn completion_is_durable_before_terminal_bytes_are_delivered() {
    let f = fixture().await;
    let service = router(&f, Arc::new(SyntheticTransport::new()));
    let mut response = service
        .execute(
            &f.principal,
            f.binding.id.clone(),
            None,
            Protocol::Responses,
            request(),
        )
        .await
        .unwrap();
    let id = response.attempt_id.clone();
    let terminal_bytes = response.stream.next().await.unwrap().unwrap();
    assert!(
        String::from_utf8(terminal_bytes.to_vec())
            .unwrap()
            .contains("response.completed")
    );
    // Do not poll EOF. A caller can disconnect as soon as it sees the terminal.
    assert_eq!(
        f.ledger.attempt(&f.principal, &id).await.unwrap().state,
        AttemptState::Succeeded
    );
    drop(response);
    assert_eq!(
        f.ledger.attempt(&f.principal, &id).await.unwrap().state,
        AttemptState::Succeeded
    );
}

struct GatedCredentials {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl CredentialStore for GatedCredentials {
    async fn load(&self, _: &CredentialRef) -> Result<SecretValue> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(SecretValue::new("fixture-secret".into()))
    }
    async fn replace(&self, _: &CredentialRef, _: SecretValue) -> Result<CredentialRef> {
        Err(Error::new(ErrorCode::Unsupported, "fixture has no refresh"))
    }
}

#[tokio::test]
async fn cancelled_http_future_does_not_orphan_admission_before_guard_creation() {
    let f = fixture().await;
    let credentials = Arc::new(GatedCredentials {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let service = Router::new(
        f.ledger.clone(),
        Arc::new(SyntheticTransport::new()),
        credentials.clone(),
        Arc::new(TestClock),
    );
    let owner = f.principal.clone();
    let binding = f.binding.id.clone();
    let operation = OperationId::new("cancelled-operation").unwrap();
    let first_operation = operation.clone();
    let task = tokio::spawn(async move {
        service
            .execute(
                &owner,
                binding,
                Some(first_operation),
                Protocol::Responses,
                request(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), credentials.entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    credentials.release.notify_one();
    // Lost response headers are recovered through the duplicate operation's ID.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let error = f
                .ledger
                .admit(
                    &f.principal,
                    Admission {
                        binding: f.binding.id.clone(),
                        operation: Some(operation.clone()),
                        request_fingerprint: "different-but-never-dispatched".into(),
                        model: "synthetic-model".into(),
                        effort: None,
                    },
                    1000,
                )
                .await
                .err()
                .unwrap();
            assert_eq!(error.code, ErrorCode::OperationConflict);
            let id = error.attempt_id.unwrap();
            if f.ledger.attempt(&f.principal, &id).await.unwrap().state == AttemptState::Uncertain {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
