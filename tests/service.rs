use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{Json, routing::get};
use poolparty::{
    config::{StateDirectory, seed_demo},
    domain::*,
    http::{BearerGrants, app},
    ports::{Clock, Ledger, SecretValue},
    providers::SyntheticTransport,
    runtime::{Router, SystemClock},
    service::{BackgroundMaintenance, ServeConfig, serve_until},
    storage::SqliteLedger,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

fn config() -> Value {
    json!({
        "inventory": {
            "state_dir":"synthetic-state", "op_executable":"/synthetic/op",
            "credentials":[{"credential":"key-a","vault":"vault-a","item":"item-a","field":"auth-json"}],
            "accounts":[{"id":"account-a","product":"codex_subscription","quota_owner":"quota-a","pool":"pool-a",
                "credential":"key-a","model":"model-a","expected_account_id":"workspace-a","max_concurrency":1,"unknown_capacity":"reject",
                "usage_url":"https://example.com/usage", "inference_url":"https://example.com/responses"}],
            "oauth":{"endpoint":"https://example.com/oauth/token","client_id":"synthetic-client"}
        },
        "grants":[{"principal":"caller-a","pools":["pool-a"],"token_env":"POOLPARTY_GRANT_CALLER_A"}]
    })
}
fn parse(value: Value) -> ServeConfig {
    serde_json::from_value(value).unwrap()
}

#[test]
fn service_defaults_to_loopback_and_requires_explicit_ingress_configuration() {
    let parsed = parse(config());
    assert_eq!(parsed.listen.to_string(), "127.0.0.1:8080");
    assert_eq!(parsed.maintenance_interval_seconds, 30);
    parsed.validate().unwrap();
    let mut value = config();
    value["listen"] = json!("0.0.0.0:8080");
    assert!(parse(value.clone()).validate().is_err());
    value["trusted_ingress"] = json!(true);
    parse(value).validate().unwrap();
}

#[test]
fn grant_configuration_separates_provider_secrets_and_requires_real_pool_scopes() {
    for name in [
        "POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN",
        "OPENAI_API_KEY",
        "POOLPARTY_GRANT_",
        "POOLPARTY_GRANT_lowercase",
    ] {
        let mut value = config();
        value["grants"][0]["token_env"] = json!(name);
        assert!(parse(value).validate().is_err());
    }
    for pools in [json!([]), json!(["other-pool"])] {
        let mut value = config();
        value["grants"][0]["pools"] = pools;
        assert!(parse(value).validate().is_err());
    }
    let mut value = config();
    value["grants"] = json!([]);
    assert!(parse(value).validate().is_err());
    for interval in [0, 4, 3601] {
        let mut value = config();
        value["maintenance_interval_seconds"] = json!(interval);
        assert!(parse(value).validate().is_err());
    }
}

#[test]
fn grants_are_resolved_only_from_named_variables_without_mutating_environment() {
    let parsed = parse(config());
    let mut names = vec![];
    let grants = parsed
        .load_grants(|name| {
            names.push(name.to_owned());
            Some(SecretValue::new(
                "synthetic-caller-grant-with-32-characters".into(),
            ))
        })
        .unwrap();
    assert_eq!(names, ["POOLPARTY_GRANT_CALLER_A"]);
    assert!(!format!("{grants:?}").contains("synthetic-caller"));
    assert!(parsed.load_grants(|_| None).is_err());
    assert!(
        parsed
            .load_grants(|_| Some(SecretValue::new("short".into())))
            .is_err()
    );
    let mut value = config();
    let mut second = value["grants"][0].clone();
    second["token_env"] = json!("POOLPARTY_GRANT_CALLER_B");
    second["principal"] = json!("caller-b");
    value["grants"].as_array_mut().unwrap().push(second);
    assert!(
        parse(value)
            .load_grants(|_| Some(SecretValue::new(
                "duplicate-synthetic-token-over-32-characters".into()
            )))
            .is_err()
    );
}

struct Maintenance {
    calls: AtomicUsize,
    entered: Notify,
    block: bool,
    fail: bool,
}
impl Maintenance {
    fn new(block: bool, fail: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            block,
            fail,
        })
    }
}
#[async_trait]
impl BackgroundMaintenance for Maintenance {
    async fn sync(&self) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.block {
            return std::future::pending().await;
        }
        if self.fail {
            return Err(Error::new(
                ErrorCode::CredentialUnavailable,
                "synthetic failure",
            ));
        }
        Ok(())
    }
}
async fn listener() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}

#[tokio::test]
async fn persistent_http_serves_authenticated_binding_stream_and_releases_state_after_drain() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("state");
    let state = Arc::new(StateDirectory::acquire(&path).unwrap());
    let ledger = Arc::new(SqliteLedger::open(&state.database).unwrap());
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    ledger.recover(clock.now()).await.unwrap();
    let (principal, credentials) = seed_demo(ledger.as_ref(), clock.now()).await.unwrap();
    let token = "synthetic-caller-grant-with-32-characters";
    let grants = BearerGrants::new(vec![(SecretValue::new(token.into()), principal)]).unwrap();
    let runtime = Arc::new(
        Router::new(
            ledger,
            Arc::new(SyntheticTransport::new()),
            Arc::new(credentials),
            clock,
        )
        .with_ownership(state.clone()),
    );
    let (listener, url) = listener().await;
    let maintenance = Maintenance::new(false, false);
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(serve_until(
        listener,
        app(runtime, grants),
        maintenance,
        Duration::from_secs(1),
        Duration::from_secs(2),
        async {
            let _ = stopped.await;
        },
    ));
    drop(state);
    assert!(StateDirectory::acquire(&path).is_err());
    let client = client();
    assert!(
        client
            .get(format!("{url}/healthz"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        client
            .get(format!("{url}/api/v1/sessions/missing"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let binding: Value = client.post(format!("{url}/api/v1/sessions")).bearer_auth(token)
        .json(&json!({"session":"session-a","pool":"demo","product":"codex_subscription","model":"synthetic-model"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let id = binding["id"].as_str().unwrap();
    let stream = client
        .post(format!("{url}/routes/{id}/codex/responses"))
        .bearer_auth(token)
        .header("x-poolparty-operation-id", "operation-a")
        .json(&json!({"model":"synthetic-model","stream":true,"input":[]}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(stream.contains("response.completed"));
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(client);
    let _reopened = StateDirectory::acquire(&path).unwrap();
}

#[tokio::test]
async fn periodic_failure_does_not_stop_listener_and_shutdown_stops_future_ticks() {
    let maintenance = Maintenance::new(false, true);
    let app = axum::Router::new().route("/healthz", get(|| async { Json(json!({"status":"ok"})) }));
    let (listener, url) = listener().await;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(serve_until(
        listener,
        app,
        maintenance.clone(),
        Duration::from_millis(20),
        Duration::from_secs(1),
        async {
            let _ = stopped.await;
        },
    ));
    tokio::time::timeout(Duration::from_secs(2), maintenance.entered.notified())
        .await
        .unwrap();
    assert!(
        client()
            .get(format!("{url}/healthz"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    let stopped_at = maintenance.calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(maintenance.calls.load(Ordering::SeqCst), stopped_at);
}

#[tokio::test]
async fn shutdown_has_a_bound_when_background_work_is_stuck() {
    let maintenance = Maintenance::new(true, false);
    let (listener, _) = listener().await;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(serve_until(
        listener,
        axum::Router::new(),
        maintenance.clone(),
        Duration::from_millis(10),
        Duration::from_millis(50),
        async {
            let _ = stopped.await;
        },
    ));
    tokio::time::timeout(Duration::from_secs(2), maintenance.entered.notified())
        .await
        .unwrap();
    stop.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StorageUnavailable);
}
