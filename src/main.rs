//! Synthetic listener and explicit one-shot credential, usage and inference checks.
use poolparty::{
    config::{StateDirectory, seed_demo},
    domain::*,
    http::{BearerGrants, app},
    ports::*,
    providers::SyntheticTransport,
    runtime::{Router, SystemClock},
    storage::SqliteLedger,
};
use std::{net::SocketAddr, sync::Arc};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("poolpartyd: {}", error.message);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(
        args.first().map(String::as_str),
        Some("--check" | "--probe" | "--refresh")
    ) {
        return run_live(&args).await;
    }
    if args.is_empty() || args == ["--help"] {
        println!(
            "Usage: poolpartyd --demo\n       poolpartyd --check CONFIG.json\n       poolpartyd --probe CONFIG.json\n       poolpartyd --refresh CONFIG.json CREDENTIAL_ID\nDemo: loopback-only, no provider calls; requires POOLPARTY_DEMO_TOKEN.\nChecks: explicit one-shot real credential/usage operations; require POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN.\nProbe: sends the configured synthetic request once per eligible account.\nRefresh: explicitly rotate one enrolled Codex credential."
        );
        return Ok(());
    }
    if args != ["--demo"] {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Unsupported command; use --help for available modes.",
        ));
    }
    let token = std::env::var("POOLPARTY_DEMO_TOKEN").map_err(|_| {
        Error::new(
            ErrorCode::InvalidInput,
            "Set POOLPARTY_DEMO_TOKEN to a random value of at least 32 characters.",
        )
    })?;
    if token.len() < 32 || token.bytes().any(|b| !b.is_ascii_graphic()) {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Demo token must contain at least 32 printable non-space ASCII characters.",
        ));
    }
    let address: SocketAddr = std::env::var("POOLPARTY_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .map_err(|_| Error::new(ErrorCode::InvalidInput, "Invalid listen address."))?;
    if !address.ip().is_loopback() {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "The synthetic demo only listens on loopback.",
        ));
    }
    let state_path = std::env::var("POOLPARTY_STATE_DIR").unwrap_or_else(|_| "state".into());
    let _state = StateDirectory::acquire(state_path)?;
    let ledger = Arc::new(SqliteLedger::open(&_state.database)?);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    ledger.recover(clock.now()).await?;
    let (principal, credentials) = seed_demo(ledger.as_ref(), clock.now()).await?;
    let grants = BearerGrants::new(vec![(SecretValue::new(token), principal)])?;
    let service = Arc::new(Router::new(
        ledger,
        Arc::new(SyntheticTransport::new()),
        Arc::new(credentials),
        clock,
    ));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|_| Error::new(ErrorCode::InvalidInput, "Cannot bind listen address."))?;
    println!(
        "Synthetic Poolparty listening on {}",
        listener
            .local_addr()
            .map_err(|_| Error::new(ErrorCode::InvalidInput, "Cannot inspect listen address."))?
    );
    axum::serve(listener, app(service, grants))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| {
            Error::new(
                ErrorCode::StorageUnavailable,
                "HTTP server stopped unexpectedly.",
            )
        })
}

async fn run_live(args: &[String]) -> Result<()> {
    use poolparty::live::{LiveConfig, LiveMode};
    let valid =
        (args[0] == "--refresh" && args.len() == 3) || (args[0] != "--refresh" && args.len() == 2);
    if !valid {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Expected CONFIG.json and, for --refresh, a credential ID.",
        ));
    }
    let path = std::path::Path::new(&args[1]);
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| Error::new(ErrorCode::InvalidInput, "Cannot read operator config."))?;
    if metadata.len() > 1024 * 1024 {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Operator config exceeds size limit.",
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| Error::new(ErrorCode::InvalidInput, "Cannot read operator config."))?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Operator config exceeds size limit.",
        ));
    }
    let config: LiveConfig = serde_json::from_slice(&bytes)
        .map_err(|_| Error::new(ErrorCode::InvalidInput, "Operator config is invalid."))?;
    let token = std::env::var("POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN").map_err(|_| {
        Error::new(
            ErrorCode::InvalidInput,
            "Set POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN for explicit vault operations.",
        )
    })?;
    let mode = match args[0].as_str() {
        "--probe" => LiveMode::Probe,
        "--refresh" => LiveMode::Refresh(
            CredentialId::new(&args[2])
                .map_err(|_| Error::new(ErrorCode::InvalidInput, "Invalid credential ID."))?,
        ),
        _ => LiveMode::Check,
    };
    let report = poolparty::live::run(config, SecretValue::new(token), mode).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|_| Error::new(ErrorCode::InvalidInput, "Cannot encode check report."))?
    );
    if report.accounts.iter().any(|a| {
        a.credential_error.is_some()
            || a.capacity == Some(CapacityStatus::ReauthenticationRequired)
            || a.collection_error.is_some()
            || a.probe
                .as_ref()
                .is_some_and(|p| p.error.is_some() || p.state != Some(AttemptState::Succeeded))
    }) {
        return Err(Error::new(
            ErrorCode::UpstreamUnavailable,
            "One or more account checks did not pass; inspect the redacted report.",
        ));
    }
    Ok(())
}
