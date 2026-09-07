//! Enrolled inventory maintenance and the pre-admission credential fence.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::{
    codex_auth::CodexAuth, domain::*, live::LiveAccount, maintenance::CredentialMaintenance,
    ports::*,
};

const FRESH_MS: i64 = 30_000;
const RETRY_MS: i64 = 5_000;
const REFRESH_SKEW_MS: i64 = 300_000;
const PREPARATION_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ManagedInventory {
    accounts: BTreeMap<AccountId, LiveAccount>,
    maintenance: CredentialMaintenance,
    store: Arc<dyn CredentialStore>,
    collector: Arc<dyn UsageCollector>,
    ledger: Arc<dyn Ledger>,
    clock: Arc<dyn Clock>,
    state: Mutex<InventoryState>,
}

#[derive(Default)]
struct InventoryState {
    credentials: BTreeMap<CredentialId, CachedCredential>,
    enrolled: BTreeMap<AccountId, CredentialRef>,
    usage_retry: BTreeMap<QuotaOwnerId, Timestamp>,
}
struct CachedCredential {
    checked_at: Timestamp,
    valid_until: Timestamp,
    result: Result<CredentialRef>,
}

impl ManagedInventory {
    pub fn new(
        accounts: Vec<LiveAccount>,
        maintenance: CredentialMaintenance,
        store: Arc<dyn CredentialStore>,
        collector: Arc<dyn UsageCollector>,
        ledger: Arc<dyn Ledger>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let mut inventory = BTreeMap::new();
        let mut credentials = BTreeMap::new();
        let mut policies = BTreeMap::new();
        for account in accounts {
            let ownership = (
                account.product,
                account.expected_account_id.clone(),
                account.quota_owner.clone(),
            );
            let policy = (account.max_concurrency, account.unknown_capacity);
            if account.model.trim().is_empty()
                || account.max_concurrency == 0
                || account.product == Product::CodexSubscription
                    && account
                        .expected_account_id
                        .as_deref()
                        .is_none_or(str::is_empty)
                || credentials
                    .insert(account.credential.clone(), ownership.clone())
                    .is_some_and(|previous| previous != ownership)
                || policies
                    .insert(account.quota_owner.clone(), policy)
                    .is_some_and(|previous| previous != policy)
                || inventory.insert(account.id.clone(), account).is_some()
            {
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    "Managed inventory has invalid or conflicting enrollment.",
                ));
            }
        }
        if inventory.is_empty() {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Managed inventory must enroll at least one account.",
            ));
        }
        Ok(Self {
            accounts: inventory,
            maintenance,
            store,
            collector,
            ledger,
            clock,
            state: Mutex::new(InventoryState::default()),
        })
    }

    /// Visit every account, retaining healthy progress even if another account fails.
    /// The first credential, storage or collection error summarizes a partial sync.
    pub async fn sync_all(&self) -> Result<()> {
        // Persisted accounts outlive configuration. Removed entries must stop
        // competing for fresh bindings before newly enrolled accounts synchronize.
        self.ledger
            .disable_unenrolled(&self.accounts.keys().cloned().collect())
            .await?;
        let mut first_error = None;
        for account in self.accounts.values() {
            match self.sync_bounded(account).await {
                Ok(Some(error)) | Err(error) => {
                    first_error.get_or_insert(error);
                }
                Ok(None) => (),
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn sync_bounded(&self, setup: &LiveAccount) -> Result<Option<Error>> {
        tokio::time::timeout(PREPARATION_TIMEOUT, async {
            let mut state = self.state.lock().await;
            self.sync_account(setup, &mut state).await
        })
        .await
        .map_err(|_| {
            Error::new(
                ErrorCode::CredentialUnavailable,
                "Account preparation exceeded its deadline; no inference was dispatched.",
            )
        })?
    }

    async fn disable_credential(
        &self,
        credential: &CredentialId,
        state: &mut InventoryState,
    ) -> Result<()> {
        let mut failure = None;
        for account in self
            .accounts
            .values()
            .filter(|account| &account.credential == credential)
        {
            state.enrolled.remove(&account.id);
            if let Err(error) = self.ledger.set_account_enabled(&account.id, false).await
                && error.code != ErrorCode::NotFound
            {
                failure.get_or_insert(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn resolve_credential(
        &self,
        setup: &LiveAccount,
        state: &mut InventoryState,
        now: Timestamp,
    ) -> Result<CredentialRef> {
        if let Some(cached) = state.credentials.get(&setup.credential)
            && cached.checked_at <= now
            && now < cached.valid_until
        {
            return cached.result.clone();
        }
        let resolved = self
            .maintenance
            .resolve(
                &setup.credential,
                setup.product,
                setup.expected_account_id.as_deref(),
                now,
                false,
            )
            .await;
        let validated = match resolved {
            Ok(reference) => match self.store.load(&reference).await {
                Ok(secret) => {
                    let valid_until = if setup.product == Product::CodexSubscription {
                        match CodexAuth::parse(&secret) {
                            Ok(auth)
                                if setup.expected_account_id.as_deref()
                                    == Some(auth.account_id()) =>
                            {
                                if auth.needs_refresh(now, REFRESH_SKEW_MS)? {
                                    Err(Error::new(
                                        ErrorCode::ReauthenticationRequired,
                                        "Managed credential is not fresh enough for admission.",
                                    ))
                                } else if auth
                                    .needs_refresh(now.saturating_add(FRESH_MS), REFRESH_SKEW_MS)?
                                {
                                    Ok(now)
                                } else {
                                    Ok(now.saturating_add(FRESH_MS))
                                }
                            }
                            Ok(_) => Err(Error::new(
                                ErrorCode::ReauthenticationRequired,
                                "Managed credential identity does not match enrollment.",
                            )),
                            Err(error) => Err(error),
                        }
                    } else {
                        Ok(now.saturating_add(FRESH_MS))
                    };
                    valid_until.map(|until| (reference, until))
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        let (result, valid_until) = match validated {
            Ok((reference, until)) => (Ok(reference), until),
            Err(error) => (Err(error), now.saturating_add(RETRY_MS)),
        };
        state.credentials.insert(
            setup.credential.clone(),
            CachedCredential {
                checked_at: now,
                valid_until,
                result: result.clone(),
            },
        );
        if result.is_err() {
            self.disable_credential(&setup.credential, state).await?;
        }
        result
    }

    async fn sync_account(
        &self,
        setup: &LiveAccount,
        state: &mut InventoryState,
    ) -> Result<Option<Error>> {
        let now = self.clock.now();
        if now < 0 {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Account preparation requires a nonnegative timestamp.",
            ));
        }
        let reference = self.resolve_credential(setup, state, now).await?;
        let account = Account {
            id: setup.id.clone(),
            product: setup.product,
            quota_owner: setup.quota_owner.clone(),
            pools: BTreeSet::from([setup.pool.clone()]),
            credential: reference.clone(),
            models: BTreeSet::from([setup.model.clone()]),
            enabled: true,
        };
        if state.enrolled.get(&setup.id) != Some(&reference) {
            self.ledger
                .put_quota_policy(QuotaPolicy {
                    owner: setup.quota_owner.clone(),
                    max_concurrency: setup.max_concurrency,
                    unknown: setup.unknown_capacity,
                })
                .await?;
            self.ledger.put_account(account.clone()).await?;
            state.enrolled.insert(setup.id.clone(), reference.clone());
        }
        let previous = self.ledger.usage_observation(&setup.quota_owner).await?;
        if state
            .usage_retry
            .get(&setup.quota_owner)
            .is_some_and(|until| now < *until)
            || previous.as_ref().is_some_and(|observation| {
                observation.observed_at <= now
                    && now
                        < observation
                            .valid_until
                            .min(observation.observed_at.saturating_add(FRESH_MS))
            })
        {
            return Ok(None);
        }
        let secret = match self.store.load(&reference).await {
            Ok(secret) => secret,
            Err(error) => {
                state.credentials.insert(
                    setup.credential.clone(),
                    CachedCredential {
                        checked_at: now,
                        valid_until: now.saturating_add(RETRY_MS),
                        result: Err(error.clone()),
                    },
                );
                self.disable_credential(&setup.credential, state).await?;
                return Err(error);
            }
        };
        let collected = self.collector.collect(&account, &secret, now).await;
        let observation = match collected {
            Ok(observation) => observation,
            Err(error) => {
                state
                    .usage_retry
                    .insert(setup.quota_owner.clone(), now.saturating_add(RETRY_MS));
                if error.code == ErrorCode::ReauthenticationRequired {
                    self.save_observation(
                        UsageObservation {
                            owner: setup.quota_owner.clone(),
                            observed_at: now,
                            valid_until: now.saturating_add(FRESH_MS),
                            provider_available: None,
                            status: CapacityStatus::ReauthenticationRequired,
                            windows: previous
                                .as_ref()
                                .map_or_else(Vec::new, |value| value.windows.clone()),
                            balances: previous
                                .as_ref()
                                .map_or_else(Vec::new, |value| value.balances.clone()),
                            source: "provider-authentication-failure".into(),
                        },
                        previous.as_ref(),
                    )
                    .await?;
                }
                // Failed collection never invents fresh available capacity or erases
                // a durable exhausted/authentication failure. Admission handles stale
                // evidence according to the explicit local unknown-capacity policy.
                return Ok(Some(error));
            }
        };
        if observation.owner != setup.quota_owner
            || observation.observed_at > now
            || observation.valid_until <= observation.observed_at
        {
            state
                .usage_retry
                .insert(setup.quota_owner.clone(), now.saturating_add(RETRY_MS));
            return Ok(Some(Error::new(
                ErrorCode::CapacityUnknown,
                "Usage collection returned invalid evidence.",
            )));
        }
        if observation.status == CapacityStatus::Unknown
            && previous.as_ref().is_some_and(|previous| {
                matches!(
                    previous.status,
                    CapacityStatus::Exhausted | CapacityStatus::ReauthenticationRequired
                )
            })
        {
            state
                .usage_retry
                .insert(setup.quota_owner.clone(), now.saturating_add(RETRY_MS));
            return Ok(Some(Error::new(
                ErrorCode::CapacityUnknown,
                "Unknown usage cannot clear a recorded account failure.",
            )));
        }
        self.save_observation(observation, previous.as_ref())
            .await?;
        state
            .usage_retry
            .insert(setup.quota_owner.clone(), now.saturating_add(RETRY_MS));
        Ok(None)
    }

    async fn save_observation(
        &self,
        observation: UsageObservation,
        previous: Option<&UsageObservation>,
    ) -> Result<()> {
        if previous.is_some_and(|previous| {
            previous.observed_at >= observation.observed_at && previous != &observation
        }) {
            return Err(Error::new(
                ErrorCode::CapacityUnknown,
                "Usage evidence did not advance the recorded observation time.",
            ));
        }
        self.ledger.observe(observation).await
    }
}

#[async_trait]
impl RequestPreparation for ManagedInventory {
    async fn prepare(&self, binding: &Binding) -> Result<()> {
        let setup = self.accounts.get(&binding.account).ok_or_else(|| {
            Error::new(
                ErrorCode::NoEligibleAccount,
                "Bound account is no longer enrolled.",
            )
        })?;
        if binding.closed_at.is_some()
            || binding.intent.product != setup.product
            || binding.intent.pool != setup.pool
            || binding.intent.model != setup.model
        {
            return Err(Error::new(
                ErrorCode::NoEligibleAccount,
                "Bound intent is not supported by current enrollment.",
            ));
        }
        self.sync_bounded(setup).await.map(|_| ())
    }
}
