//! Persistent listener wiring. Provider secrets remain in managed inventory custody.
use std::{
    collections::BTreeSet,
    future::{Future, IntoFuture},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::{net::TcpListener, sync::watch};

use crate::{
    codex_auth::CodexRefresher,
    config::StateDirectory,
    domain::*,
    http::{BearerGrants, app_with_readiness},
    live::{LiveConfig, inventory_endpoints, validate_inventory},
    maintenance::CredentialMaintenance,
    managed::ManagedInventory,
    onepassword::OnePasswordStore,
    ports::{Clock, Ledger, SecretValue},
    providers::HttpTransport,
    readiness::Readiness,
    runtime::{Router, SystemClock},
    storage::SqliteLedger,
    usage::HttpUsageCollector,
};

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8080))
}
fn default_interval() -> u64 {
    30
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServeConfig {
    pub inventory: LiveConfig,
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    pub grants: Vec<GrantConfig>,
    #[serde(default = "default_interval")]
    pub maintenance_interval_seconds: u64,
    /// Explicitly acknowledge that a trusted ingress protects a non-loopback socket.
    #[serde(default)]
    pub trusted_ingress: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantConfig {
    pub principal: PrincipalId,
    pub pools: BTreeSet<PoolId>,
    /// Names only. Values are obtained from POOLPARTY_GRANT_* environment variables.
    pub token_env: String,
}

impl ServeConfig {
    pub fn validate(&self) -> Result<()> {
        validate_inventory(&self.inventory)?;
        inventory_endpoints(&self.inventory.accounts)?;
        if !self.listen.ip().is_loopback() && !self.trusted_ingress {
            return Err(invalid(
                "Non-loopback listeners require trusted_ingress=true and an authenticated HTTPS ingress.",
            ));
        }
        if !(5..=3600).contains(&self.maintenance_interval_seconds) {
            return Err(invalid(
                "Maintenance interval must be between 5 and 3600 seconds.",
            ));
        }
        if self.grants.is_empty() {
            return Err(invalid("At least one caller grant must be configured."));
        }
        let configured_pools: BTreeSet<_> = self
            .inventory
            .accounts
            .iter()
            .map(|account| &account.pool)
            .collect();
        let mut names = BTreeSet::new();
        for grant in &self.grants {
            let suffix = grant.token_env.strip_prefix("POOLPARTY_GRANT_");
            if suffix.is_none_or(|suffix| {
                suffix.is_empty()
                    || !suffix
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            }) || !names.insert(&grant.token_env)
                || grant.pools.is_empty()
                || grant
                    .pools
                    .iter()
                    .any(|pool| !configured_pools.contains(pool))
            {
                return Err(invalid(
                    "Grants require unique POOLPARTY_GRANT_* names and configured nonempty pool scopes.",
                ));
            }
        }
        Ok(())
    }

    /// Resolver injection lets tests exercise grants without mutating process environment.
    pub fn load_grants(
        &self,
        mut resolve: impl FnMut(&str) -> Option<SecretValue>,
    ) -> Result<BearerGrants> {
        self.validate()?;
        let mut grants = Vec::new();
        for configured in &self.grants {
            let token = resolve(&configured.token_env).ok_or_else(|| {
                invalid("A configured caller grant environment variable is missing.")
            })?;
            if token.expose().len() < 32 {
                return Err(invalid(
                    "Caller grants require at least 32 visible ASCII characters.",
                ));
            }
            grants.push((
                token,
                Principal {
                    id: configured.principal.clone(),
                    pools: configured.pools.clone(),
                },
            ));
        }
        BearerGrants::new(grants)
    }
}

fn invalid(message: &'static str) -> Error {
    Error::new(ErrorCode::InvalidInput, message)
}
fn service_error(message: &'static str) -> Error {
    Error::new(ErrorCode::StorageUnavailable, message)
}

pub async fn run(config: ServeConfig, service_token: SecretValue) -> Result<()> {
    config.validate()?;
    // A caller grant must never double as the vault service credential.
    let mut reused_service_token = false;
    let grants = config.load_grants(|name| {
        let token = std::env::var(name).ok()?;
        reused_service_token |= token == service_token.expose();
        Some(SecretValue::new(token))
    })?;
    if reused_service_token {
        return Err(invalid(
            "Caller and vault service credentials must be distinct.",
        ));
    }
    let mapped = validate_inventory(&config.inventory)?;
    let (usage_endpoints, inference_endpoints) = inventory_endpoints(&config.inventory.accounts)?;
    let store = Arc::new(OnePasswordStore::new(
        config.inventory.credentials.clone(),
        service_token,
        config.inventory.op_executable.clone(),
    )?);
    let refresher = Arc::new(CodexRefresher::new(
        config.inventory.oauth.endpoint.clone(),
        config.inventory.oauth.client_id.clone(),
        false,
    )?);
    let collector = Arc::new(HttpUsageCollector::new(usage_endpoints, false)?);
    let transport = Arc::new(HttpTransport::new(inference_endpoints, false)?);
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|_| invalid("Cannot bind the configured listener."))?;
    let state = Arc::new(StateDirectory::acquire(&config.inventory.state_dir)?);
    let ledger = Arc::new(SqliteLedger::open(&state.database)?.with_ownership(state.clone()));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    ledger.recover(clock.now()).await?;
    let maintenance = CredentialMaintenance::new(
        store.clone(),
        refresher,
        state.clone(),
        ledger.clone(),
        mapped.into_iter().collect(),
    )?;
    let managed = Arc::new(ManagedInventory::new(
        config.inventory.accounts,
        maintenance,
        store.clone(),
        collector,
        ledger.clone(),
        clock.clone(),
    )?);
    if let Err(error) = managed.sync_all().await {
        if matches!(
            error.code,
            ErrorCode::StorageUnavailable | ErrorCode::InvalidInput
        ) {
            return Err(error);
        }
        // Unavailable upstream accounts remain fenced while healthy enrollments serve.
        eprintln!("Poolparty initial maintenance incomplete: {:?}", error.code);
    }
    let readiness = Arc::new(Readiness::new(ledger.clone()));
    let runtime = Arc::new(
        Router::new(ledger, transport, store, clock)
            .with_preparation(managed.clone())
            .with_ownership(state.clone()),
    );
    println!(
        "Poolparty listening on {}",
        listener
            .local_addr()
            .map_err(|_| invalid("Cannot inspect listen address."))?
    );
    serve_until(
        listener,
        app_with_readiness(runtime, grants, readiness.clone()),
        managed,
        Duration::from_secs(config.maintenance_interval_seconds),
        Duration::from_secs(30),
        shutdown_signal(),
        Some(readiness),
    )
    .await
}

#[async_trait]
pub trait BackgroundMaintenance: Send + Sync {
    async fn sync(&self) -> Result<()>;
}
#[async_trait]
impl BackgroundMaintenance for ManagedInventory {
    async fn sync(&self) -> Result<()> {
        self.sync_all().await
    }
}

/// Stop accepting, drain HTTP and finish maintenance within one bounded deadline.
/// A timeout is fatal to the daemon process. Runtime ownership guards keep its
/// state lock alive until any detached work has stopped or the process exits.
pub async fn serve_until(
    listener: TcpListener,
    app: axum::Router,
    maintenance: Arc<dyn BackgroundMaintenance>,
    interval: Duration,
    drain: Duration,
    shutdown: impl Future<Output = ()> + Send,
    readiness: Option<Arc<Readiness>>,
) -> Result<()> {
    if interval.is_zero() || drain.is_zero() {
        return Err(invalid("Maintenance and drain durations must be positive."));
    }
    let _lifecycle = readiness.as_ref().map(Readiness::serving);
    let (stop, mut stop_worker) = watch::channel(false);
    let mut stop_server = stop.subscribe();
    let mut worker = tokio::spawn(async move {
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = stop_worker.changed() => break,
                _ = timer.tick() => {
                    if let Err(error) = maintenance.sync().await {
                        eprintln!("Poolparty maintenance failed: {:?}", error.code);
                    }
                }
            }
        }
    });
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            if !*stop_server.borrow() {
                let _ = stop_server.changed().await;
            }
        })
        .into_future();
    tokio::pin!(serving);
    tokio::pin!(shutdown);
    let completed = tokio::select! {
        result = &mut serving => Some(result),
        _ = &mut shutdown => None,
    };
    if let Some(readiness) = &readiness {
        readiness.stop();
    }
    let _ = stop.send(true);
    let deadline = tokio::time::Instant::now() + drain;
    let server_result = match completed {
        Some(result) => result.map_err(|_| service_error("HTTP listener stopped unexpectedly.")),
        None => match tokio::time::timeout_at(deadline, &mut serving).await {
            Ok(result) => result.map_err(|_| service_error("HTTP listener stopped unexpectedly.")),
            Err(_) => Err(service_error(
                "HTTP drain deadline exceeded; restart will fence unresolved work.",
            )),
        },
    };
    let worker_result = match tokio::time::timeout_at(deadline, &mut worker).await {
        Ok(result) => result.map_err(|_| service_error("Background maintenance task failed.")),
        Err(_) => {
            worker.abort();
            let _ = worker.await;
            Err(service_error(
                "Maintenance drain deadline exceeded; inspect pending credential state on restart.",
            ))
        }
    };
    server_result?;
    worker_result
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                _ = tokio::signal::ctrl_c() => (),
                _ = terminate.recv() => (),
            },
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
