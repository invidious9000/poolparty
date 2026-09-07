use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use poolparty::{
    config::{MemoryCredentials, StateDirectory},
    domain::Result,
    http::{BearerGrants, app_with_readiness},
    providers::SyntheticTransport,
    readiness::Readiness,
    runtime::{Router, SystemClock},
    service::{BackgroundMaintenance, serve_until},
    storage::SqliteLedger,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};
use tower::ServiceExt;

struct Maintenance {
    block: bool,
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl BackgroundMaintenance for Maintenance {
    async fn sync(&self) -> Result<()> {
        self.entered.notify_one();
        if self.block {
            self.release.notified().await;
        }
        Ok(())
    }
}

async fn response(app: &axum::Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn readiness_tracks_local_storage_and_service_drain_without_provider_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("state");
    let state = Arc::new(StateDirectory::acquire(path).unwrap());
    let ledger = Arc::new(
        SqliteLedger::open(&state.database)
            .unwrap()
            .with_ownership(state.clone()),
    );
    let readiness = Arc::new(Readiness::new(ledger.clone()));
    let runtime = Arc::new(Router::new(
        ledger,
        Arc::new(SyntheticTransport::new()),
        Arc::new(MemoryCredentials::default()),
        Arc::new(SystemClock),
    ));
    let app = app_with_readiness(
        runtime,
        BearerGrants::new(vec![]).unwrap(),
        readiness.clone(),
    );
    assert_eq!(
        response(&app, "/readyz").await,
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"status":"not_ready"})
        )
    );
    assert_eq!(response(&app, "/healthz").await.0, StatusCode::OK);
    let maintenance = Arc::new(Maintenance {
        block: true,
        entered: Notify::new(),
        release: Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(serve_until(
        listener,
        app.clone(),
        maintenance.clone(),
        Duration::from_millis(10),
        Duration::from_secs(2),
        async {
            let _ = stopped.await;
        },
        Some(readiness.clone()),
    ));
    tokio::time::timeout(Duration::from_secs(2), maintenance.entered.notified())
        .await
        .unwrap();
    // No accounts, credentials or provider capacity are needed for local readiness.
    assert_eq!(
        response(&app, "/readyz").await,
        (StatusCode::OK, json!({"status":"ready"}))
    );
    assert_eq!(
        response(&app, "/api/v1/accounts").await.0,
        StatusCode::UNAUTHORIZED
    );
    let connection = rusqlite::Connection::open(&state.database).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        response(&app, "/readyz").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(response(&app, "/healthz").await.0, StatusCode::OK);
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(response(&app, "/readyz").await.0, StatusCode::OK);
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while readiness.ready().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    assert_eq!(
        response(&app, "/readyz").await,
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"status":"not_ready"})
        )
    );
    maintenance.release.notify_one();
    task.await.unwrap().unwrap();
    assert!(!readiness.ready().await);
}

#[tokio::test]
async fn cancelling_service_future_clears_readiness_even_without_shutdown_signal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("state");
    let state = Arc::new(StateDirectory::acquire(path).unwrap());
    let ledger = Arc::new(
        SqliteLedger::open(&state.database)
            .unwrap()
            .with_ownership(state),
    );
    let readiness = Arc::new(Readiness::new(ledger));
    let maintenance = Arc::new(Maintenance {
        block: false,
        entered: Notify::new(),
        release: Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let task = tokio::spawn(serve_until(
        listener,
        axum::Router::new(),
        maintenance.clone(),
        Duration::from_millis(10),
        Duration::from_secs(1),
        std::future::pending(),
        Some(readiness.clone()),
    ));
    tokio::time::timeout(Duration::from_secs(2), maintenance.entered.notified())
        .await
        .unwrap();
    assert!(readiness.ready().await);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!readiness.ready().await);
}
