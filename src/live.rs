//! Shared private inventory and explicit one-shot credential/usage checks.
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use crate::{
    codex_auth::CodexRefresher,
    config::StateDirectory,
    domain::*,
    maintenance::CredentialMaintenance,
    onepassword::{OnePasswordStore, VaultField},
    ports::*,
    providers::{Endpoint, HttpTransport},
    runtime::{Router, SystemClock},
    storage::SqliteLedger,
    usage::{HttpUsageCollector, UsageEndpoint},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveConfig {
    pub state_dir: PathBuf,
    pub op_executable: PathBuf,
    pub credentials: Vec<VaultField>,
    pub accounts: Vec<LiveAccount>,
    pub oauth: OAuthConfig,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthConfig {
    pub endpoint: String,
    pub client_id: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveAccount {
    pub id: AccountId,
    pub product: Product,
    pub quota_owner: QuotaOwnerId,
    pub pool: PoolId,
    pub credential: CredentialId,
    pub model: String,
    pub expected_account_id: Option<String>,
    pub max_concurrency: u32,
    pub unknown_capacity: UnknownCapacityPolicy,
    pub usage_url: Option<String>,
    pub inference_url: Option<String>,
    /// A deliberately supplied tiny synthetic request; never a stored conversation.
    pub probe: Option<serde_json::Value>,
}

pub enum LiveMode {
    Check,
    Probe,
    Refresh(CredentialId),
}

#[derive(Serialize)]
pub struct CheckReport {
    pub accounts: Vec<AccountReport>,
}
#[derive(Serialize)]
pub struct AccountReport {
    pub account: AccountId,
    pub product: Product,
    pub credential_generation: Option<u64>,
    pub credential_error: Option<ErrorCode>,
    pub capacity: Option<CapacityStatus>,
    pub window_count: usize,
    pub collection_error: Option<ErrorCode>,
    pub probe: Option<ProbeReport>,
}
#[derive(Serialize)]
pub struct ProbeReport {
    pub binding: Option<BindingId>,
    pub attempt: Option<AttemptId>,
    pub http_status: Option<u16>,
    pub content_type: Option<String>,
    pub byte_count: usize,
    pub state: Option<AttemptState>,
    pub error: Option<ErrorCode>,
}

pub async fn run(
    config: LiveConfig,
    service_token: SecretValue,
    mode: LiveMode,
) -> Result<CheckReport> {
    let mapped = validate_inventory(&config)?;
    if let LiveMode::Refresh(id) = &mode
        && !config
            .accounts
            .iter()
            .any(|a| a.credential == *id && a.product == Product::CodexSubscription)
    {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Refresh target is not an enrolled Codex credential.",
        ));
    }
    let state = Arc::new(StateDirectory::acquire(&config.state_dir)?);
    let ledger = Arc::new(SqliteLedger::open(&state.database)?.with_ownership(state.clone()));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    ledger.recover(clock.now()).await?;
    let store = Arc::new(OnePasswordStore::new(
        config.credentials,
        service_token,
        config.op_executable,
    )?);
    let refresher = Arc::new(CodexRefresher::new(
        config.oauth.endpoint,
        config.oauth.client_id,
        false,
    )?);
    let maintenance = CredentialMaintenance::new(
        store.clone(),
        refresher,
        state,
        ledger.clone(),
        mapped.into_iter().collect(),
    )?;
    let (usage_endpoints, inference_endpoints) = inventory_endpoints(&config.accounts)?;
    let collector = HttpUsageCollector::new(usage_endpoints, false)?;
    let transport = Arc::new(HttpTransport::new(inference_endpoints, false)?);
    let router = Router::new(ledger.clone(), transport, store.clone(), clock.clone());
    let mut reports = Vec::new();
    // Cache failures as well as successes. An alias cannot retry issuance or
    // reconcile an ambiguous exchange from earlier in the same invocation.
    let mut resolved: BTreeMap<CredentialId, Result<CredentialRef>> = BTreeMap::new();
    for setup in config.accounts {
        if let LiveMode::Refresh(id) = &mode
            && setup.credential != *id
        {
            continue;
        }
        let mut report = AccountReport {
            account: setup.id.clone(),
            product: setup.product,
            credential_generation: None,
            credential_error: None,
            capacity: None,
            window_count: 0,
            collection_error: None,
            probe: None,
        };
        let result = match resolved.get(&setup.credential) {
            Some(result) => result.clone(),
            None => {
                let result = maintenance
                    .resolve(
                        &setup.credential,
                        setup.product,
                        setup.expected_account_id.as_deref(),
                        clock.now(),
                        matches!(mode, LiveMode::Refresh(_)),
                    )
                    .await;
                resolved.insert(setup.credential.clone(), result.clone());
                result
            }
        };
        let reference = match result {
            Ok(reference) => reference,
            Err(error) => {
                report.credential_error = Some(error.code);
                reports.push(report);
                continue;
            }
        };
        report.credential_generation = Some(reference.generation);
        let account = Account {
            id: setup.id.clone(),
            product: setup.product,
            quota_owner: setup.quota_owner.clone(),
            pools: BTreeSet::from([setup.pool.clone()]),
            credential: reference.clone(),
            models: BTreeSet::from([setup.model.clone()]),
            enabled: true,
        };
        ledger.put_account(account.clone()).await?;
        ledger
            .put_quota_policy(QuotaPolicy {
                owner: setup.quota_owner.clone(),
                max_concurrency: setup.max_concurrency,
                unknown: setup.unknown_capacity,
            })
            .await?;
        let secret = store.load(&reference).await?;
        match collector.collect(&account, &secret, clock.now()).await {
            Ok(observation) => {
                report.capacity = Some(observation.status);
                report.window_count = observation.windows.len();
                ledger.observe(observation).await?;
            }
            Err(error) => {
                report.collection_error = Some(error.code);
            }
        }
        if matches!(mode, LiveMode::Probe)
            && let Some(body) = setup.probe
        {
            let principal = Principal {
                id: PrincipalId::new("operator-check").expect("constant"),
                pools: BTreeSet::from([setup.pool.clone()]),
            };
            let session = ClientSessionId::new(uuid::Uuid::new_v4().to_string()).expect("UUID");
            let created = ledger
                .create_binding(
                    &principal,
                    CreateBinding {
                        session,
                        pool: setup.pool,
                        product: setup.product,
                        model: setup.model,
                        account: Some(setup.id),
                        effort: None,
                    },
                    clock.now(),
                )
                .await;
            let mut probe = ProbeReport {
                binding: None,
                attempt: None,
                http_status: None,
                content_type: None,
                byte_count: 0,
                state: None,
                error: None,
            };
            match created {
                Err(error) => probe.error = Some(error.code),
                Ok(binding) => {
                    probe.binding = Some(binding.id.clone());
                    let operation =
                        OperationId::new(uuid::Uuid::new_v4().to_string()).expect("UUID");
                    let bytes = Bytes::from(serde_json::to_vec(&body).map_err(|_| {
                        Error::new(ErrorCode::InvalidInput, "Invalid probe request.")
                    })?);
                    match router
                        .execute(
                            &principal,
                            binding.id.clone(),
                            Some(operation),
                            setup.product.protocol(),
                            bytes,
                        )
                        .await
                    {
                        Err(error) => {
                            probe.error = Some(error.code);
                            probe.attempt = error.attempt_id;
                        }
                        Ok(mut response) => {
                            probe.http_status = Some(response.status);
                            probe.content_type = response
                                .headers
                                .iter()
                                .find(|(name, _)| name == "content-type")
                                .map(|(_, value)| value.clone());
                            probe.attempt = Some(response.attempt_id.clone());
                            let drained = tokio::time::timeout(Duration::from_secs(60), async {
                                while let Some(chunk) = response.stream.next().await {
                                    probe.byte_count += chunk?.len();
                                    if probe.byte_count > 256 * 1024 {
                                        return Err(Error::new(
                                            ErrorCode::UpstreamUnavailable,
                                            "Probe output bound reached.",
                                        ));
                                    }
                                }
                                Ok(())
                            })
                            .await;
                            match drained {
                                Ok(Ok(())) => (),
                                Ok(Err(error)) => probe.error = Some(error.code),
                                Err(_) => probe.error = Some(ErrorCode::UpstreamUnavailable),
                            }
                            drop(response);
                        }
                    }
                    if let Some(id) = &probe.attempt {
                        let observed =
                            await_probe_settlement(ledger.as_ref(), &principal, id).await?;
                        probe.state = Some(observed);
                        if matches!(
                            observed,
                            AttemptState::Reserved
                                | AttemptState::Dispatching
                                | AttemptState::Streaming
                        ) {
                            // Report the actual durable state if cleanup could not finish.
                            probe.error.get_or_insert(ErrorCode::StorageUnavailable);
                        }
                    }
                    ledger
                        .close_binding(&principal, &binding.id, clock.now())
                        .await?;
                }
            }
            report.probe = Some(probe);
        }
        reports.push(report);
    }
    Ok(CheckReport { accounts: reports })
}

async fn await_probe_settlement(
    ledger: &dyn Ledger,
    principal: &Principal,
    id: &AttemptId,
) -> Result<AttemptState> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut last_observed = None;
    loop {
        let state = match tokio::time::timeout_at(deadline, ledger.attempt(principal, id)).await {
            Ok(attempt) => attempt?.state,
            Err(_) => {
                return last_observed.ok_or_else(|| {
                    Error::new(
                        ErrorCode::StorageUnavailable,
                        "Probe settlement could not be inspected within the deadline.",
                    )
                });
            }
        };
        last_observed = Some(state);
        if !matches!(
            state,
            AttemptState::Reserved | AttemptState::Dispatching | AttemptState::Streaming
        ) || tokio::time::Instant::now() >= deadline
        {
            return Ok(state);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Validate all aliases before state acquisition or external credential operations.
pub fn validate_inventory(config: &LiveConfig) -> Result<BTreeSet<CredentialId>> {
    if config.accounts.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "At least one account must be explicitly configured.",
        ));
    }
    let mut account_ids = BTreeSet::new();
    let mut credential_owners = BTreeMap::new();
    let mut quota_policies = BTreeMap::new();
    let mapped: BTreeSet<_> = config
        .credentials
        .iter()
        .map(|field| field.credential.clone())
        .collect();
    for account in &config.accounts {
        if !account_ids.insert(account.id.clone())
            || !mapped.contains(&account.credential)
            || account.model.trim().is_empty()
            || account.max_concurrency == 0
        {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Invalid account inventory.",
            ));
        }
        if account.product == Product::CodexSubscription
            && account
                .expected_account_id
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Codex requires an explicitly enrolled upstream account identity.",
            ));
        }
        let ownership = (
            account.product,
            account.expected_account_id.as_deref(),
            &account.quota_owner,
        );
        if let Some(previous) = credential_owners.insert(&account.credential, ownership)
            && previous != ownership
        {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Credential aliases must agree on product, upstream account identity and quota owner.",
            ));
        }
        let policy = (account.max_concurrency, account.unknown_capacity);
        if let Some(previous) = quota_policies.insert(&account.quota_owner, policy)
            && previous != policy
        {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Accounts sharing a quota owner must agree on admission policy.",
            ));
        }
    }
    Ok(mapped)
}

pub(crate) fn inventory_endpoints(
    accounts: &[LiveAccount],
) -> Result<(Vec<UsageEndpoint>, Vec<Endpoint>)> {
    // Product endpoints are trusted configuration, never caller-controlled URLs.
    let mut usage_endpoints = Vec::new();
    let mut inference_endpoints = Vec::new();
    for account in accounts {
        if let Some(url) = &account.usage_url {
            if let Some(existing) = usage_endpoints
                .iter()
                .find(|e: &&UsageEndpoint| e.product == account.product)
            {
                if existing.url != *url {
                    return Err(Error::new(
                        ErrorCode::InvalidInput,
                        "Conflicting product usage endpoints.",
                    ));
                }
            } else {
                usage_endpoints.push(UsageEndpoint {
                    product: account.product,
                    url: url.clone(),
                });
            }
        }
        if let Some(url) = &account.inference_url {
            if let Some(existing) = inference_endpoints
                .iter()
                .find(|e: &&Endpoint| e.product == account.product)
            {
                if existing.url != *url {
                    return Err(Error::new(
                        ErrorCode::InvalidInput,
                        "Conflicting product inference endpoints.",
                    ));
                }
            } else {
                inference_endpoints.push(Endpoint {
                    product: account.product,
                    url: url.clone(),
                });
            }
        }
    }
    Ok((usage_endpoints, inference_endpoints))
}
