use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use poolparty::{domain::*, ports::Ledger, storage::SqliteLedger};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
    ledger: SqliteLedger,
    principal: Principal,
}

fn account(name: &str, owner: &str) -> Account {
    Account {
        id: AccountId::new(name).unwrap(),
        product: Product::CodexSubscription,
        quota_owner: QuotaOwnerId::new(owner).unwrap(),
        pools: BTreeSet::from([PoolId::new("pool-a").unwrap()]),
        credential: CredentialRef {
            id: CredentialId::new(format!("credential-{name}")).unwrap(),
            generation: 1,
        },
        models: BTreeSet::from(["model-a".to_owned()]),
        enabled: true,
    }
}
fn policy(owner: &str, cap: u32, unknown: UnknownCapacityPolicy) -> QuotaPolicy {
    QuotaPolicy {
        owner: QuotaOwnerId::new(owner).unwrap(),
        max_concurrency: cap,
        unknown,
    }
}
fn observation(status: CapacityStatus, at: Timestamp, until: Timestamp) -> UsageObservation {
    UsageObservation {
        owner: QuotaOwnerId::new("owner-a").unwrap(),
        observed_at: at,
        valid_until: until,
        status,
        windows: vec![],
        balances: vec![],
        source: "synthetic".to_owned(),
        provider_available: None,
    }
}
fn intent(session: &str, account: Option<&str>) -> CreateBinding {
    CreateBinding {
        session: ClientSessionId::new(session).unwrap(),
        pool: PoolId::new("pool-a").unwrap(),
        product: Product::CodexSubscription,
        model: "model-a".to_owned(),
        account: account.map(|value| AccountId::new(value).unwrap()),
        effort: Some("high".to_owned()),
    }
}
fn admission(binding: &Binding, operation: Option<&str>) -> Admission {
    Admission {
        binding: binding.id.clone(),
        operation: operation.map(|value| OperationId::new(value).unwrap()),
        request_fingerprint: "synthetic-sha256".to_owned(),
        model: binding.intent.model.clone(),
        effort: binding.intent.effort.clone(),
    }
}

impl Fixture {
    async fn new(cap: u32) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("ledger.sqlite");
        let ledger = SqliteLedger::open(&path).unwrap();
        ledger
            .put_quota_policy(policy(
                "owner-a",
                cap,
                UnknownCapacityPolicy::AllowUnderLocalCap,
            ))
            .await
            .unwrap();
        ledger
            .put_account(account("account-a", "owner-a"))
            .await
            .unwrap();
        Self {
            _directory: directory,
            path,
            ledger,
            principal: Principal {
                id: PrincipalId::new("principal-a").unwrap(),
                pools: BTreeSet::from([PoolId::new("pool-a").unwrap()]),
            },
        }
    }
    async fn bind(&self, session: &str) -> Binding {
        self.ledger
            .create_binding(&self.principal, intent(session, None), 10)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn racing_admissions_across_aliases_share_one_quota_owner() {
    let fixture = Fixture::new(3).await;
    fixture
        .ledger
        .put_account(account("account-b", "owner-a"))
        .await
        .unwrap();
    let binding_a = fixture
        .ledger
        .create_binding(
            &fixture.principal,
            intent("session-a", Some("account-a")),
            10,
        )
        .await
        .unwrap();
    let binding_b = fixture
        .ledger
        .create_binding(
            &fixture.principal,
            intent("session-b", Some("account-b")),
            10,
        )
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(20));
    let mut tasks = Vec::new();
    for number in 0..20 {
        // Independent connections exercise SQLite's transaction fence as well as the local mutex.
        let ledger = SqliteLedger::open(&fixture.path).unwrap();
        let principal = fixture.principal.clone();
        let request = admission(
            if number % 2 == 0 {
                &binding_a
            } else {
                &binding_b
            },
            Some(&format!("operation-{number}")),
        );
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            ledger.admit(&principal, request, 20).await
        }));
    }
    let mut accepted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => accepted += 1,
            Err(error) => assert_eq!(error.code, ErrorCode::SessionConcurrencyExhausted),
        }
    }
    assert_eq!(accepted, 3);
}

#[tokio::test]
async fn racing_session_creation_is_idempotent_and_hard_intent_is_immutable() {
    let fixture = Fixture::new(1).await;
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let ledger = SqliteLedger::open(&fixture.path).unwrap();
        let principal = fixture.principal.clone();
        tasks.push(tokio::spawn(async move {
            ledger
                .create_binding(&principal, intent("same-session", None), 10)
                .await
                .unwrap()
        }));
    }
    let first = tasks.remove(0).await.unwrap();
    for task in tasks {
        assert_eq!(task.await.unwrap(), first);
    }
    let mut changed = first.intent.clone();
    changed.effort = Some("low".to_owned());
    let error = fixture
        .ledger
        .create_binding(&fixture.principal, changed, 20)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IntentConflict);
    assert_eq!(error.binding_id, Some(first.id));
}

#[tokio::test]
async fn principal_and_revoked_pool_cannot_discover_bindings_or_attempts() {
    let fixture = Fixture::new(2).await;
    let binding = fixture.bind("session-a").await;
    let prepared = fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 20)
        .await
        .unwrap();
    let foreign = Principal {
        id: PrincipalId::new("principal-b").unwrap(),
        pools: fixture.principal.pools.clone(),
    };
    let revoked = Principal {
        id: fixture.principal.id.clone(),
        pools: BTreeSet::new(),
    };
    for principal in [&foreign, &revoked] {
        let error = fixture
            .ledger
            .binding(principal, &binding.id)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.binding_id, None);
        assert_eq!(
            fixture
                .ledger
                .attempt(principal, &prepared.attempt.id)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            fixture
                .ledger
                .admit(principal, admission(&binding, None), 20)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            fixture
                .ledger
                .close_binding(principal, &binding.id, 20)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
    assert_eq!(
        fixture
            .ledger
            .create_binding(&revoked, intent("new-session", None), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unauthorized
    );
    let own = fixture
        .ledger
        .create_binding(&foreign, binding.intent.clone(), 20)
        .await
        .unwrap();
    assert_ne!(own.id, binding.id);
    let mut other_pool = fixture.principal.clone();
    other_pool.pools = BTreeSet::from([PoolId::new("pool-b").unwrap()]);
    let mut changed_pool = binding.intent.clone();
    changed_pool.pool = PoolId::new("pool-b").unwrap();
    assert_eq!(
        fixture
            .ledger
            .create_binding(&other_pool, changed_pool, 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn restart_preserves_affinity_and_fences_possibly_dispatched_work() {
    let fixture = Fixture::new(4).await;
    let reserved_binding = fixture.bind("reserved").await;
    let sent_binding = fixture.bind("sent").await;
    let streaming_binding = fixture.bind("streaming").await;
    let reserved = fixture
        .ledger
        .admit(
            &fixture.principal,
            admission(&reserved_binding, Some("reserved-op")),
            20,
        )
        .await
        .unwrap()
        .attempt;
    let sent = fixture
        .ledger
        .admit(
            &fixture.principal,
            admission(&sent_binding, Some("sent-op")),
            20,
        )
        .await
        .unwrap()
        .attempt;
    let streaming = fixture
        .ledger
        .admit(&fixture.principal, admission(&streaming_binding, None), 20)
        .await
        .unwrap()
        .attempt;
    fixture.ledger.mark_dispatching(&sent.id, 21).await.unwrap();
    fixture
        .ledger
        .mark_dispatching(&streaming.id, 21)
        .await
        .unwrap();
    fixture
        .ledger
        .mark_streaming(&streaming.id, 22)
        .await
        .unwrap();
    drop(fixture.ledger);
    let ledger = SqliteLedger::open(&fixture.path).unwrap();
    ledger.recover(30).await.unwrap();
    ledger.recover(31).await.unwrap();
    assert_eq!(
        ledger
            .binding(&fixture.principal, &sent_binding.id)
            .await
            .unwrap(),
        sent_binding
    );
    assert_eq!(
        ledger
            .attempt(&fixture.principal, &reserved.id)
            .await
            .unwrap()
            .state,
        AttemptState::NotDispatched
    );
    for attempt in [&sent, &streaming] {
        assert_eq!(
            ledger
                .attempt(&fixture.principal, &attempt.id)
                .await
                .unwrap()
                .state,
            AttemptState::Uncertain
        );
    }
    let error = ledger
        .admit(&fixture.principal, admission(&sent_binding, None), 100_000)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SessionUncertain);
    assert_eq!(error.attempt_id, Some(sent.id.clone()));
    assert!(error.binding_preserved);
    assert_eq!(
        ledger
            .admit(
                &fixture.principal,
                admission(&reserved_binding, Some("reserved-op")),
                40
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationAlreadyExists
    );
    ledger
        .admit(
            &fixture.principal,
            admission(&reserved_binding, Some("fresh-op")),
            40,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn exhaustion_and_auth_failure_survive_expiry_until_newer_evidence() {
    let fixture = Fixture::new(1).await;
    let binding = fixture.bind("session-a").await;
    for (at, status, expected) in [
        (
            20,
            CapacityStatus::Exhausted,
            ErrorCode::SessionQuotaExhausted,
        ),
        (
            30,
            CapacityStatus::ReauthenticationRequired,
            ErrorCode::ReauthenticationRequired,
        ),
    ] {
        fixture
            .ledger
            .observe(observation(status, at, at + 1))
            .await
            .unwrap();
        let error = fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 100)
            .await
            .unwrap_err();
        assert_eq!(error.code, expected);
        assert_eq!(error.binding_id, Some(binding.id.clone()));
        assert!(error.binding_preserved);
        assert_eq!(
            fixture
                .ledger
                .observe(observation(CapacityStatus::Available, at - 1, 200))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        assert_eq!(
            fixture
                .ledger
                .observe(observation(CapacityStatus::Available, at, 200))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
    }
    fixture
        .ledger
        .observe(observation(CapacityStatus::Available, 40, 200))
        .await
        .unwrap();
    fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 100)
        .await
        .unwrap();
}

#[tokio::test]
async fn unknown_and_stale_capacity_follow_explicit_policy() {
    let fixture = Fixture::new(1).await;
    let binding = fixture.bind("session-a").await;
    fixture
        .ledger
        .put_quota_policy(policy("owner-a", 1, UnknownCapacityPolicy::Reject))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::CapacityUnknown
    );
    fixture
        .ledger
        .observe(observation(CapacityStatus::Available, 20, 30))
        .await
        .unwrap();
    for now in [19, 30, 100] {
        assert_eq!(
            fixture
                .ledger
                .admit(&fixture.principal, admission(&binding, None), now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::CapacityUnknown
        );
    }
    fixture
        .ledger
        .observe(observation(CapacityStatus::Unknown, 25, 50))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 30)
            .await
            .unwrap_err()
            .code,
        ErrorCode::CapacityUnknown
    );
    fixture
        .ledger
        .put_quota_policy(policy(
            "owner-a",
            1,
            UnknownCapacityPolicy::AllowUnderLocalCap,
        ))
        .await
        .unwrap();
    fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 30)
        .await
        .unwrap();
}

#[tokio::test]
async fn invalid_caps_and_observation_times_are_rejected() {
    let fixture = Fixture::new(1).await;
    assert_eq!(
        fixture
            .ledger
            .put_quota_policy(policy("owner-a", 0, UnknownCapacityPolicy::Reject))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    for (at, until) in [(-1, 20), (20, 20), (20, 19)] {
        assert_eq!(
            fixture
                .ledger
                .observe(observation(CapacityStatus::Available, at, until))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
    }
    let observation = observation(CapacityStatus::Available, 20, 30);
    fixture.ledger.observe(observation.clone()).await.unwrap();
    fixture.ledger.observe(observation).await.unwrap();
}

#[tokio::test]
async fn closed_session_tombstones_prevent_recreation_and_missing_resume_fails() {
    let fixture = Fixture::new(1).await;
    let binding = fixture.bind("session-a").await;
    let closed = fixture
        .ledger
        .close_binding(&fixture.principal, &binding.id, 20)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .ledger
            .close_binding(&fixture.principal, &binding.id, 30)
            .await
            .unwrap(),
        closed
    );
    assert_eq!(
        fixture
            .ledger
            .binding(&fixture.principal, &binding.id)
            .await
            .unwrap(),
        closed
    );
    assert_eq!(
        fixture
            .ledger
            .create_binding(&fixture.principal, binding.intent.clone(), 30)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 30)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    let mut missing = admission(&binding, None);
    missing.binding = BindingId::new("missing").unwrap();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, missing, 30)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn account_ownership_cannot_change_and_credential_rotation_preserves_binding() {
    let fixture = Fixture::new(2).await;
    let binding = fixture.bind("session-a").await;
    let before = fixture
        .ledger
        .admit(
            &fixture.principal,
            admission(&binding, Some("before-rotation")),
            20,
        )
        .await
        .unwrap();
    let mut changed = account("account-a", "owner-a");
    changed.product = Product::GlmCoding;
    assert_eq!(
        fixture.ledger.put_account(changed).await.unwrap_err().code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        fixture
            .ledger
            .put_account(account("account-a", "owner-b"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    let mut rotated = account("account-a", "owner-a");
    rotated.credential.generation = 2;
    fixture.ledger.put_account(rotated.clone()).await.unwrap();
    let after = fixture
        .ledger
        .admit(
            &fixture.principal,
            admission(&binding, Some("after-rotation")),
            30,
        )
        .await
        .unwrap();
    assert_eq!(before.binding, after.binding);
    assert_eq!(before.attempt.credential.generation, 1);
    assert_eq!(after.attempt.credential.generation, 2);
    assert_eq!(
        fixture
            .ledger
            .put_account(account("account-a", "owner-a"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    rotated.credential.id = CredentialId::new("replacement").unwrap();
    rotated.credential.generation = 1;
    fixture.ledger.put_account(rotated).await.unwrap();
    assert_eq!(
        fixture
            .ledger
            .put_account(account("account-a", "owner-a"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn current_account_membership_and_model_are_checked_at_every_admission() {
    let fixture = Fixture::new(1).await;
    let binding = fixture.bind("session-a").await;
    let mut updated = account("account-a", "owner-a");
    updated.enabled = false;
    fixture.ledger.put_account(updated).await.unwrap();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NoEligibleAccount
    );
    let mut updated = account("account-a", "owner-a");
    updated.pools.clear();
    fixture.ledger.put_account(updated).await.unwrap();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NoEligibleAccount
    );
    let mut updated = account("account-a", "owner-a");
    updated.models = BTreeSet::from(["model-b".to_owned()]);
    fixture.ledger.put_account(updated).await.unwrap();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NoEligibleAccount
    );
    // Re-creating the original session never reallocates it because policy changed.
    assert_eq!(
        fixture
            .ledger
            .create_binding(&fixture.principal, binding.intent.clone(), 20)
            .await
            .unwrap(),
        binding
    );
    let mut request = admission(&binding, None);
    request.model = "model-b".to_owned();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, request, 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntentConflict
    );
}

#[tokio::test]
async fn duplicate_operations_never_dispatch_again_and_report_previous_certainty() {
    let fixture = Fixture::new(2).await;
    let binding = fixture.bind("session-a").await;
    let request = admission(&binding, Some("operation-a"));
    let prepared = fixture
        .ledger
        .admit(&fixture.principal, request.clone(), 20)
        .await
        .unwrap();
    for (state, certainty) in [
        (AttemptState::Reserved, DispatchCertainty::NotDispatched),
        (AttemptState::Dispatching, DispatchCertainty::Unknown),
        (AttemptState::Succeeded, DispatchCertainty::Dispatched),
    ] {
        match state {
            AttemptState::Dispatching => fixture
                .ledger
                .mark_dispatching(&prepared.attempt.id, 21)
                .await
                .unwrap(),
            AttemptState::Succeeded => {
                fixture
                    .ledger
                    .settle(&prepared.attempt.id, Settlement::Succeeded, 22)
                    .await
                    .unwrap();
            }
            _ => (),
        }
        let error = fixture
            .ledger
            .admit(&fixture.principal, request.clone(), 30)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::OperationAlreadyExists);
        assert_eq!(error.request_state, certainty);
        assert_eq!(error.attempt_id, Some(prepared.attempt.id.clone()));
        let mut conflict = request.clone();
        conflict.request_fingerprint = "different-sha256".to_owned();
        assert_eq!(
            fixture
                .ledger
                .admit(&fixture.principal, conflict, 30)
                .await
                .unwrap_err()
                .code,
            ErrorCode::OperationConflict
        );
    }
    let another = fixture.bind("session-b").await;
    fixture
        .ledger
        .admit(
            &fixture.principal,
            admission(&another, Some("operation-a")),
            30,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn settlement_requires_evidence_and_terminal_completion_is_idempotent() {
    let fixture = Fixture::new(1).await;
    let binding = fixture.bind("session-a").await;
    let first = fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 20)
        .await
        .unwrap()
        .attempt;
    assert_eq!(
        fixture
            .ledger
            .mark_streaming(&first.id, 21)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidTransition
    );
    for outcome in [
        Settlement::Succeeded,
        Settlement::Rejected,
        Settlement::Uncertain,
    ] {
        assert_eq!(
            fixture
                .ledger
                .settle(&first.id, outcome, 21)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidTransition
        );
    }
    fixture
        .ledger
        .mark_dispatching(&first.id, 21)
        .await
        .unwrap();
    // Explicit transport evidence of pre-send failure can release recorded intent.
    let terminal = fixture
        .ledger
        .settle(&first.id, Settlement::NotDispatched, 22)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .ledger
            .settle(&first.id, Settlement::NotDispatched, 23)
            .await
            .unwrap(),
        terminal
    );
    assert_eq!(
        fixture
            .ledger
            .settle(&first.id, Settlement::Succeeded, 23)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidTransition
    );
    let second = fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 30)
        .await
        .unwrap()
        .attempt;
    fixture
        .ledger
        .mark_dispatching(&second.id, 31)
        .await
        .unwrap();
    fixture.ledger.mark_streaming(&second.id, 32).await.unwrap();
    assert_eq!(
        fixture
            .ledger
            .settle(&second.id, Settlement::NotDispatched, 33)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidTransition
    );
    fixture
        .ledger
        .settle(&second.id, Settlement::Uncertain, 33)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 34)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionUncertain
    );
    fixture
        .ledger
        .settle(&second.id, Settlement::Succeeded, 35)
        .await
        .unwrap();
    fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 36)
        .await
        .unwrap();
}

#[tokio::test]
async fn lowering_cap_retains_pressure_and_new_session_can_select_another_owner() {
    let fixture = Fixture::new(2).await;
    let binding = fixture.bind("session-a").await;
    let first = fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, Some("first")), 20)
        .await
        .unwrap()
        .attempt;
    let second = fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, Some("second")), 20)
        .await
        .unwrap()
        .attempt;
    fixture
        .ledger
        .put_quota_policy(policy(
            "owner-a",
            1,
            UnknownCapacityPolicy::AllowUnderLocalCap,
        ))
        .await
        .unwrap();
    fixture
        .ledger
        .settle(&first.id, Settlement::NotDispatched, 21)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, Some("third")), 22)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionConcurrencyExhausted
    );
    fixture
        .ledger
        .put_account(account("account-b", "owner-b"))
        .await
        .unwrap();
    fixture
        .ledger
        .put_quota_policy(policy(
            "owner-b",
            1,
            UnknownCapacityPolicy::AllowUnderLocalCap,
        ))
        .await
        .unwrap();
    let fresh = fixture
        .ledger
        .create_binding(&fixture.principal, intent("session-b", None), 22)
        .await
        .unwrap();
    assert_eq!(fresh.account.as_str(), "account-b");
    assert_eq!(
        fixture
            .ledger
            .binding(&fixture.principal, &binding.id)
            .await
            .unwrap()
            .account
            .as_str(),
        "account-a"
    );
    fixture
        .ledger
        .settle(&second.id, Settlement::NotDispatched, 23)
        .await
        .unwrap();
    fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 24)
        .await
        .unwrap();
}

#[tokio::test]
async fn native_requests_remain_exclusive_before_drop_cleanup_and_with_mixed_operation_ids() {
    let fixture = Fixture::new(2).await;
    let binding = fixture.bind("session-a").await;
    let native = fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 20)
        .await
        .unwrap()
        .attempt;
    for state in [
        AttemptState::Reserved,
        AttemptState::Dispatching,
        AttemptState::Streaming,
    ] {
        match state {
            AttemptState::Dispatching => fixture
                .ledger
                .mark_dispatching(&native.id, 21)
                .await
                .unwrap(),
            AttemptState::Streaming => fixture.ledger.mark_streaming(&native.id, 22).await.unwrap(),
            _ => (),
        }
        for operation in [None, Some("explicit-next")] {
            let error = fixture
                .ledger
                .admit(&fixture.principal, admission(&binding, operation), 23)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::SessionConcurrencyExhausted);
            assert_eq!(error.request_state, DispatchCertainty::NotDispatched);
            assert_eq!(error.attempt_id, Some(native.id.clone()));
            assert_eq!(error.binding_id, Some(binding.id.clone()));
        }
    }
    fixture
        .ledger
        .settle(&native.id, Settlement::Succeeded, 24)
        .await
        .unwrap();
    let explicit = fixture
        .ledger
        .admit(
            &fixture.principal,
            admission(&binding, Some("explicit-first")),
            25,
        )
        .await
        .unwrap()
        .attempt;
    let error = fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 26)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SessionConcurrencyExhausted);
    assert_eq!(error.attempt_id, Some(explicit.id));
    // Two distinct explicit operations can still consume the two owner slots.
    fixture
        .ledger
        .admit(
            &fixture.principal,
            admission(&binding, Some("explicit-second")),
            26,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn credential_watermark_is_durable_idempotent_and_rejects_rollback() {
    let fixture = Fixture::new(1).await;
    let reference = CredentialRef {
        id: CredentialId::new("unenrolled-credential").unwrap(),
        generation: 7,
    };
    fixture
        .ledger
        .advance_credential_generation(&reference)
        .await
        .unwrap();
    fixture
        .ledger
        .advance_credential_generation(&reference)
        .await
        .unwrap();
    let newer = CredentialRef {
        generation: 8,
        ..reference.clone()
    };
    fixture
        .ledger
        .advance_credential_generation(&newer)
        .await
        .unwrap();
    drop(fixture.ledger);
    let reopened = SqliteLedger::open(&fixture.path).unwrap();
    assert_eq!(
        reopened
            .advance_credential_generation(&reference)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    reopened
        .advance_credential_generation(&newer)
        .await
        .unwrap();
    // Numeric comparison must cover the entire u64 domain, rather than SQLite's
    // signed integer range or lexicographic ordering of the stored decimal text.
    let maximum = CredentialRef {
        generation: u64::MAX,
        ..reference
    };
    reopened
        .advance_credential_generation(&maximum)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .advance_credential_generation(&newer)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn account_upserts_and_admission_honor_maintenance_watermarks() {
    let fixture = Fixture::new(1).await;
    let binding = fixture.bind("session-a").await;
    let mut newer = account("account-a", "owner-a");
    newer.credential.generation = 3;
    fixture
        .ledger
        .advance_credential_generation(&newer.credential)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .ledger
            .put_account(account("account-a", "owner-a"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        fixture
            .ledger
            .admit(&fixture.principal, admission(&binding, None), 20)
            .await
            .unwrap_err()
            .code,
        ErrorCode::CredentialUnavailable
    );
    fixture.ledger.put_account(newer).await.unwrap();
    let prepared = fixture
        .ledger
        .admit(&fixture.principal, admission(&binding, None), 20)
        .await
        .unwrap();
    assert_eq!(prepared.attempt.credential.generation, 3);
    assert_eq!(prepared.binding, binding);
}

#[tokio::test]
async fn existing_credential_metadata_fences_upgrades_before_watermark_population() {
    let fixture = Fixture::new(1).await;
    // Simulate an existing ledger from before this additive table was introduced.
    let connection = rusqlite::Connection::open(&fixture.path).unwrap();
    connection
        .execute("DELETE FROM credential_watermarks", [])
        .unwrap();
    drop(connection);
    let previous = account("account-a", "owner-a").credential;
    let rollback = CredentialRef {
        generation: 0,
        ..previous.clone()
    };
    let reopened = SqliteLedger::open(&fixture.path).unwrap();
    assert_eq!(
        reopened
            .advance_credential_generation(&rollback)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    reopened
        .advance_credential_generation(&previous)
        .await
        .unwrap();
}

#[tokio::test]
async fn account_listing_filters_principal_visibility_and_hides_other_pool_memberships() {
    let fixture = Fixture::new(1).await;
    let mut shared = account("account-a", "owner-a");
    shared.pools.insert(PoolId::new("pool-b").unwrap());
    fixture.ledger.put_account(shared.clone()).await.unwrap();
    let mut private = account("account-private", "owner-private");
    private.pools = BTreeSet::from([PoolId::new("pool-b").unwrap()]);
    private.enabled = false;
    fixture.ledger.put_account(private.clone()).await.unwrap();
    let visible = fixture.ledger.accounts(&fixture.principal).await.unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, shared.id);
    assert_eq!(visible[0].pools, fixture.principal.pools);
    let other = Principal {
        id: PrincipalId::new("principal-b").unwrap(),
        pools: BTreeSet::from([PoolId::new("pool-b").unwrap()]),
    };
    let visible = fixture.ledger.accounts(&other).await.unwrap();
    assert_eq!(visible.len(), 2);
    assert!(visible.iter().all(|account| account.pools == other.pools));
    assert!(
        visible
            .iter()
            .any(|account| account.id == private.id && !account.enabled)
    );
    let revoked = Principal {
        id: fixture.principal.id.clone(),
        pools: BTreeSet::new(),
    };
    assert!(fixture.ledger.accounts(&revoked).await.unwrap().is_empty());
    // Listing is a projection; it must not delete hidden memberships in storage.
    let both = Principal {
        id: PrincipalId::new("operator").unwrap(),
        pools: shared.pools.clone(),
    };
    let visible = fixture.ledger.accounts(&both).await.unwrap();
    assert_eq!(
        visible
            .into_iter()
            .find(|account| account.id == shared.id)
            .unwrap()
            .pools,
        shared.pools
    );
}
