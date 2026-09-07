//! Loopback-only synthetic executable; production provider configuration is not enabled.
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
    if args.is_empty() || args == ["--help"] {
        println!(
            "Usage: poolpartyd --demo\nLoopback-only synthetic router. No provider calls.\nRequired: POOLPARTY_DEMO_TOKEN (at least 32 characters)\nOptional: POOLPARTY_STATE_DIR (default ./state), POOLPARTY_LISTEN (default 127.0.0.1:8080)"
        );
        return Ok(());
    }
    if args != ["--demo"] {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Only --demo is implemented.",
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
