use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use bytes::Bytes;
use futures_util::stream;
use poolparty::{
    domain::*,
    http::{BearerGrants, app},
    ports::*,
    runtime::Router,
    storage::SqliteLedger,
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        100
    }
}
struct Credentials;
#[async_trait]
impl CredentialStore for Credentials {
    async fn load(&self, _: &CredentialRef) -> Result<SecretValue> {
        Ok(SecretValue::new("synthetic-provider-key".into()))
    }
    async fn replace(&self, _: &CredentialRef, _: SecretValue) -> Result<CredentialRef> {
        unreachable!()
    }
}

struct FakeTransport {
    calls: AtomicUsize,
    fail_stream: bool,
}
#[async_trait]
impl Transport for FakeTransport {
    async fn send(
        &self,
        request: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse, TransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.secret.expose(), "synthetic-provider-key");
        let data = match request.protocol {
            Protocol::Messages => {
                Bytes::from_static(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
            }
            Protocol::Responses => Bytes::from_static(
                b"event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n",
            ),
        };
        let events = if self.fail_stream {
            vec![
                Ok(StreamEvent::Data(Bytes::from_static(b"partial"))),
                Err(TransportError {
                    certainty: DispatchCertainty::Unknown,
                    code: ErrorCode::UpstreamUnavailable,
                }),
            ]
        } else {
            vec![
                Ok(StreamEvent::Data(data)),
                Ok(StreamEvent::Finished(Settlement::Succeeded)),
            ]
        };
        Ok(UpstreamResponse {
            status: 200,
            headers: vec![
                ("content-type".into(), "text/event-stream".into()),
                ("x-request-id".into(), "synthetic-upstream-request".into()),
                ("authorization".into(), "synthetic-must-not-escape".into()),
                ("set-cookie".into(), "synthetic-must-not-escape".into()),
                (
                    "location".into(),
                    "https://example.com/private-origin".into(),
                ),
                ("x-poolparty-attempt-id".into(), "spoofed".into()),
            ],
            stream: Box::pin(stream::iter(events)),
        })
    }
}

fn principal(id: &str, pools: &[&str]) -> Principal {
    Principal {
        id: PrincipalId::new(id).unwrap(),
        pools: pools.iter().map(|s| PoolId::new(*s).unwrap()).collect(),
        admin: false,
    }
}
fn grants() -> BearerGrants {
    BearerGrants::new(vec![
        (
            SecretValue::new("caller-a-token".into()),
            principal("caller-a", &["pool-a"]),
        ),
        (
            SecretValue::new("caller-b-token".into()),
            principal("caller-b", &["pool-a"]),
        ),
        (
            SecretValue::new("revoked-pool-token".into()),
            principal("caller-a", &["pool-b"]),
        ),
        (
            SecretValue::new("caller-ab-token".into()),
            principal("caller-ab", &["pool-a", "pool-b"]),
        ),
    ])
    .unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    ledger: Arc<SqliteLedger>,
    app: axum::Router,
    transport: Arc<FakeTransport>,
    product: Product,
}
impl Fixture {
    async fn new(product: Product, fail_stream: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(SqliteLedger::open(dir.path().join("ledger.sqlite")).unwrap());
        let owner = QuotaOwnerId::new("quota-a").unwrap();
        ledger
            .put_quota_policy(QuotaPolicy {
                owner: owner.clone(),
                max_concurrency: 1,
                unknown: UnknownCapacityPolicy::Reject,
            })
            .await
            .unwrap();
        ledger
            .put_account(Account {
                id: AccountId::new("account-a").unwrap(),
                product,
                quota_owner: owner.clone(),
                pools: BTreeSet::from([PoolId::new("pool-a").unwrap()]),
                credential: CredentialRef {
                    id: CredentialId::new("key-a").unwrap(),
                    generation: 1,
                },
                models: BTreeSet::from(["model-a".into()]),
                enabled: true,
            })
            .await
            .unwrap();
        ledger
            .observe(UsageObservation {
                owner,
                observed_at: 1,
                valid_until: 1000,
                status: CapacityStatus::Available,
                windows: vec![],
                balances: vec![],
                source: "synthetic".into(),
                provider_available: None,
            })
            .await
            .unwrap();
        let transport = Arc::new(FakeTransport {
            calls: AtomicUsize::new(0),
            fail_stream,
        });
        let runtime = Arc::new(Router::new(
            ledger.clone(),
            transport.clone(),
            Arc::new(Credentials),
            Arc::new(FixedClock),
        ));
        Self {
            _dir: dir,
            ledger,
            app: app(runtime, grants()),
            transport,
            product,
        }
    }
    fn intent(&self) -> Value {
        json!({"session":"session-a", "pool":"pool-a", "product":self.product, "model":"model-a"})
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> axum::response::Response {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        self.app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
    }
    async fn create(&self) -> Binding {
        let response = self
            .request(
                "POST",
                "/api/v1/sessions",
                Some("caller-a-token"),
                self.intent(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        serde_json::from_value(value(response).await).unwrap()
    }
    fn route(&self, id: &BindingId) -> String {
        match self.product.protocol() {
            Protocol::Responses => format!("/routes/{id}/codex/responses"),
            Protocol::Messages => format!("/routes/{id}/v1/messages"),
        }
    }
}
async fn value(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 3 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn accounts_require_auth_and_project_only_authorized_inventory_and_usage() {
    let fixture = Fixture::new(Product::GlmCoding, false).await;
    let mut visible = fixture
        .ledger
        .accounts(&principal("caller-a", &["pool-a"]))
        .await
        .unwrap()
        .remove(0);
    visible.pools.insert(PoolId::new("pool-hidden").unwrap());
    visible.credential.generation = 7;
    visible.enabled = false;
    fixture.ledger.put_account(visible.clone()).await.unwrap();
    let mut hidden = visible.clone();
    hidden.id = AccountId::new("hidden-account").unwrap();
    hidden.pools = BTreeSet::from([PoolId::new("pool-hidden").unwrap()]);
    hidden.credential.id = CredentialId::new("hidden-credential").unwrap();
    hidden.quota_owner = QuotaOwnerId::new("hidden-owner").unwrap();
    fixture.ledger.put_account(hidden).await.unwrap();
    fixture
        .ledger
        .observe(UsageObservation {
            owner: visible.quota_owner.clone(),
            observed_at: 90,
            valid_until: 1000,
            status: CapacityStatus::Exhausted,
            windows: vec![UsageWindow {
                key: "primary".into(),
                unit: "percent".into(),
                used: Some(100),
                limit: Some(100),
                resets_at: Some(999),
                used_percent: Some("100".into()),
                window_seconds: Some(3600),
            }],
            balances: vec![],
            source: "synthetic".into(),
            provider_available: Some(false),
        })
        .await
        .unwrap();
    let response = fixture
        .request("GET", "/api/v1/accounts", None, Value::Null)
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = fixture
        .request(
            "GET",
            "/api/v1/accounts",
            Some("caller-a-token"),
            Value::Null,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = value(response).await;
    let accounts = body["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    let account = &accounts[0];
    assert_eq!(account["id"], "account-a");
    assert_eq!(account["product"], "glm_coding");
    assert_eq!(account["pools"], json!(["pool-a"]));
    assert_eq!(account["models"], json!(["model-a"]));
    assert_eq!(account["enabled"], false);
    assert_eq!(account["credential_generation"], 7);
    assert_eq!(account["quota_owner"], "quota-a");
    assert_eq!(account["usage"]["status"], "exhausted");
    assert_eq!(account["usage"]["windows"][0]["resets_at"], 999);
    for forbidden in [
        "pool-hidden",
        "hidden-account",
        "hidden-owner",
        "hidden-credential",
        "key-a",
        "synthetic-provider-key",
    ] {
        assert!(!body.to_string().contains(forbidden));
    }
    assert!(account.get("credential").is_none());
    let response = fixture
        .request(
            "GET",
            "/api/v1/accounts",
            Some("revoked-pool-token"),
            Value::Null,
        )
        .await;
    assert_eq!(value(response).await, json!({"accounts":[]}));
}

#[tokio::test]
async fn accounts_preserve_absent_usage_as_unknown_instead_of_inventing_capacity() {
    let fixture = Fixture::new(Product::GlmCoding, false).await;
    fixture
        .ledger
        .put_account(Account {
            id: AccountId::new("account-unobserved").unwrap(),
            product: Product::GlmCoding,
            quota_owner: QuotaOwnerId::new("quota-unobserved").unwrap(),
            pools: BTreeSet::from([PoolId::new("pool-a").unwrap()]),
            credential: CredentialRef {
                id: CredentialId::new("key-unobserved").unwrap(),
                generation: 1,
            },
            models: BTreeSet::from(["model-a".into()]),
            enabled: true,
        })
        .await
        .unwrap();
    let response = fixture
        .request(
            "GET",
            "/api/v1/accounts",
            Some("caller-a-token"),
            Value::Null,
        )
        .await;
    let body = value(response).await;
    let account = body["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["id"] == "account-unobserved")
        .unwrap();
    assert!(account["usage"].is_null());
}
fn model_body() -> Value {
    json!({"model":"model-a", "stream":true, "messages":[]})
}

#[test]
fn grant_validation_and_debug_do_not_disclose_tokens() {
    let make = |token: &str| {
        (
            SecretValue::new(token.into()),
            principal("caller-a", &["pool-a"]),
        )
    };
    for token in ["", " ", "token with spaces", "token\n"] {
        assert!(BearerGrants::new(vec![make(token)]).is_err());
    }
    assert!(BearerGrants::new(vec![make("same"), make("same")]).is_err());
    let grant = BearerGrants::new(vec![make("hidden-token")]).unwrap();
    assert!(!format!("{grant:?}").contains("hidden-token"));
}

#[tokio::test]
async fn authentication_precedes_body_parsing_and_health_is_public() {
    let fixture = Fixture::new(Product::GlmCoding, false).await;
    let response = fixture.request("GET", "/healthz", None, Value::Null).await;
    assert_eq!(value(response).await, json!({"status":"ok"}));
    for token in [None, Some("wrong-token")] {
        let response = fixture
            .request(
                "POST",
                "/api/v1/sessions",
                token,
                json!({"principal":"forged"}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
        assert_eq!(value(response).await["error"]["code"], "unauthorized");
    }
    let response = fixture
        .request("POST", "/routes/missing/v1/messages", None, Value::Null)
        .await;
    assert_eq!(value(response).await["type"], "error");
    for authorization in [
        "bearer caller-a-token",
        "Bearer  caller-a-token",
        "Bearer caller-a-token ",
    ] {
        let request = Request::builder()
            .uri("/api/v1/sessions/missing")
            .header("authorization", authorization)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            fixture.app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let request = Request::builder()
        .uri("/api/v1/sessions/missing")
        .header("authorization", "Bearer caller-a-token")
        .header("authorization", "Bearer caller-b-token")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        fixture.app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn all_bound_routes_recheck_identity_and_pool_membership() {
    let fixture = Fixture::new(Product::GlmCoding, false).await;
    let binding = fixture.create().await;
    for token in ["caller-b-token", "revoked-pool-token"] {
        for (method, path) in [
            ("GET", format!("/api/v1/sessions/{}", binding.id)),
            ("POST", format!("/api/v1/sessions/{}/close", binding.id)),
            ("POST", fixture.route(&binding.id)),
            (
                "POST",
                format!("{}/count_tokens", fixture.route(&binding.id)),
            ),
        ] {
            let response = fixture
                .request(method, &path, Some(token), model_body())
                .await;
            assert!(matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED
            ));
            let body = value(response).await;
            assert!(body["error"]["binding_id"].is_null());
        }
    }
    assert!(
        fixture
            .ledger
            .binding(&principal("caller-a", &["pool-a"]), &binding.id)
            .await
            .unwrap()
            .closed_at
            .is_none()
    );
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn conflicting_create_and_injected_principal_are_rejected() {
    let fixture = Fixture::new(Product::GlmCoding, false).await;
    fixture.create().await;
    let mut intent = fixture.intent();
    intent["model"] = json!("different-model");
    let response = fixture
        .request("POST", "/api/v1/sessions", Some("caller-a-token"), intent)
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(value(response).await["error"]["code"], "intent_conflict");
    let mut intent = fixture.intent();
    intent["principal"] = json!("caller-b");
    let response = fixture
        .request("POST", "/api/v1/sessions", Some("caller-a-token"), intent)
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn stream_preserves_bytes_filters_headers_and_exposes_owned_attempt() {
    for product in [Product::GlmCoding, Product::CodexSubscription] {
        let fixture = Fixture::new(product, false).await;
        let binding = fixture.create().await;
        let request = Request::builder()
            .method("POST")
            .uri(fixture.route(&binding.id))
            .header("authorization", "Bearer caller-a-token")
            .header("x-poolparty-operation-id", "operation-a")
            .body(Body::from(model_body().to_string()))
            .unwrap();
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let attempt = response.headers()["x-poolparty-attempt-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert_ne!(attempt, "spoofed");
        for header in ["authorization", "set-cookie", "location"] {
            assert!(!response.headers().contains_key(header));
        }
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let expected = if product.protocol() == Protocol::Messages {
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        } else {
            "event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n"
        };
        assert_eq!(bytes.as_ref(), expected.as_bytes());
        let path = format!("/api/v1/attempts/{attempt}");
        let response = fixture
            .request("GET", &path, Some("caller-a-token"), Value::Null)
            .await;
        let attempt = value(response).await;
        assert_eq!(attempt["operation"], "operation-a");
        assert_eq!(attempt["state"], "succeeded");
        for token in ["caller-b-token", "revoked-pool-token"] {
            let response = fixture
                .request("GET", &path, Some(token), Value::Null)
                .await;
            assert!(matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED
            ));
        }
        assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn exhausted_resume_keeps_binding_and_protocol_error_shape() {
    for product in [Product::GlmCoding, Product::CodexSubscription] {
        let fixture = Fixture::new(product, false).await;
        let binding = fixture.create().await;
        fixture
            .ledger
            .observe(UsageObservation {
                owner: QuotaOwnerId::new("quota-a").unwrap(),
                observed_at: 90,
                valid_until: 1000,
                status: CapacityStatus::Exhausted,
                windows: vec![],
                balances: vec![],
                source: "synthetic".into(),
                provider_available: None,
            })
            .await
            .unwrap();
        let response = fixture
            .request(
                "POST",
                &fixture.route(&binding.id),
                Some("caller-a-token"),
                model_body(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = value(response).await;
        assert_eq!(body["error"]["code"], "session_quota_exhausted");
        assert_eq!(body["error"]["binding_id"], binding.id.as_str());
        assert_eq!(body["error"]["binding_preserved"], true);
        assert_eq!(body["error"]["request_state"], "not_dispatched");
        assert_eq!(body["error"]["type"], "rate_limit_error");
        if product.protocol() == Protocol::Messages {
            assert_eq!(body["type"], "error");
        }
        assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn unsupported_websocket_and_compaction_never_dispatch() {
    let fixture = Fixture::new(Product::CodexSubscription, false).await;
    let binding = fixture.create().await;
    for (method, path) in [
        ("GET", fixture.route(&binding.id)),
        ("POST", format!("{}/compact", fixture.route(&binding.id))),
    ] {
        let response = fixture
            .request(method, &path, Some("caller-a-token"), model_body())
            .await;
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(value(response).await["error"]["binding_preserved"], true);
    }
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn operation_validation_and_body_cap_fail_before_dispatch() {
    let fixture = Fixture::new(Product::GlmCoding, false).await;
    let binding = fixture.create().await;
    for operation in ["bad operation", "", "bad/slash"] {
        let request = Request::builder()
            .method("POST")
            .uri(fixture.route(&binding.id))
            .header("authorization", "Bearer caller-a-token")
            .header("x-poolparty-operation-id", operation)
            .body(Body::from(model_body().to_string()))
            .unwrap();
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let response = fixture
        .request(
            "POST",
            &fixture.route(&binding.id),
            Some("caller-a-token"),
            json!({"padding":"x".repeat(2 * 1024 * 1024)}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(value(response).await["error"]["code"], "invalid_input");
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn post_header_failure_interrupts_the_body_and_fences_resume() {
    let fixture = Fixture::new(Product::GlmCoding, true).await;
    let binding = fixture.create().await;
    let response = fixture
        .request(
            "POST",
            &fixture.route(&binding.id),
            Some("caller-a-token"),
            model_body(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(to_bytes(response.into_body(), 4096).await.is_err());
    let response = fixture
        .request(
            "POST",
            &fixture.route(&binding.id),
            Some("caller-a-token"),
            model_body(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(value(response).await["error"]["code"], "session_uncertain");
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn session_creation_reports_pool_exhaustion_with_exclusions() {
    let fixture = Fixture::new(Product::CodexSubscription, false).await;
    fixture
        .ledger
        .observe(UsageObservation {
            owner: QuotaOwnerId::new("quota-a").unwrap(),
            observed_at: 90,
            valid_until: 1000,
            status: CapacityStatus::Exhausted,
            windows: vec![],
            balances: vec![],
            source: "synthetic".into(),
            provider_available: None,
        })
        .await
        .unwrap();
    for pinned in [Value::Null, json!("account-a")] {
        let mut intent = fixture.intent();
        intent["account"] = pinned;
        let response = fixture
            .request("POST", "/api/v1/sessions", Some("caller-a-token"), intent)
            .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = value(response).await;
        assert_eq!(body["error"]["code"], "session_quota_exhausted");
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["binding_id"], Value::Null);
        assert_eq!(body["error"]["binding_preserved"], false);
        assert_eq!(body["error"]["request_state"], "not_dispatched");
        assert_eq!(
            body["error"]["exclusions"],
            json!([{"account":"account-a","code":"session_quota_exhausted","message":"account quota is exhausted"}])
        );
    }
    let mut intent = fixture.intent();
    intent["account"] = json!("account-b");
    let response = fixture
        .request("POST", "/api/v1/sessions", Some("caller-a-token"), intent)
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = value(response).await;
    assert_eq!(body["error"]["code"], "no_eligible_account");
    assert!(body["error"].get("exclusions").is_none());
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 0);
}

async fn drain(response: axum::response::Response) -> axum::http::HeaderMap {
    let headers = response.headers().clone();
    to_bytes(response.into_body(), 3 * 1024 * 1024)
        .await
        .unwrap();
    headers
}

fn pool_request(
    fixture: &Fixture,
    path: &str,
    token: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let _ = fixture;
    request.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn drop_in_route_binds_native_threads_without_client_bookkeeping() {
    let fixture = Fixture::new(Product::CodexSubscription, false).await;
    let body =
        json!({"model":"model-a", "stream":true, "input":"hello", "reasoning":{"effort":"low"}});
    let thread = [("thread-id", "0192aaaa-1111-7bbb-8ccc-dddddddddddd")];

    // First turn creates the binding; the response names the account and binding.
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &thread,
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let headers = drain(response).await;
    assert_eq!(headers["x-poolparty-account"], "account-a");
    let binding = headers["x-poolparty-binding"].to_str().unwrap().to_owned();
    let record: Binding = serde_json::from_value(
        value(
            fixture
                .request(
                    "GET",
                    &format!("/api/v1/sessions/{binding}"),
                    Some("caller-a-token"),
                    Value::Null,
                )
                .await,
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        record.intent.session.as_str(),
        "native-0192aaaa-1111-7bbb-8ccc-dddddddddddd"
    );
    assert_eq!(record.intent.effort, None);
    assert_eq!(record.intent.model, None);
    assert_eq!(record.intent.account, None);

    // A resumed thread sends the same header and lands on the same binding.
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &thread,
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        drain(response).await["x-poolparty-binding"],
        binding.as_str()
    );
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 2);

    // Effort passes through per request on the same binding; nothing is pinned.
    let mut changed = body.clone();
    changed["reasoning"] = json!({"effort":"high"});
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &thread,
            changed,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        drain(response).await["x-poolparty-binding"],
        binding.as_str()
    );

    // A model the bound account does not serve is refused on that binding, not moved.
    let mut other_model = body.clone();
    other_model["model"] = json!("model-b");
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &thread,
            other_model,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let error = value(response).await;
    assert_eq!(error["error"]["code"], "no_eligible_account");
    assert_eq!(error["error"]["binding_id"], binding.as_str());
    assert_eq!(error["error"]["request_state"], "not_dispatched");

    // The session-id header is the fallback; a request with neither is rejected before dispatch.
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &[("session-id", "conn-1")],
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_ne!(
        drain(response).await["x-poolparty-binding"],
        binding.as_str()
    );
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &[],
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(value(response).await["error"]["code"], "invalid_input");

    // An account pin outside the grant and the wrong protocol surface never dispatch.
    for (path, headers) in [
        (
            "/v1/responses",
            vec![("thread-id", "t2"), ("x-poolparty-account", "account-z")],
        ),
        ("/v1/messages", vec![("thread-id", "t3")]),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(pool_request(
                &fixture,
                path,
                "caller-a-token",
                &headers,
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            value(response).await["error"]["code"],
            "no_eligible_account"
        );
    }
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn drop_in_route_requires_an_explicit_pool_when_several_qualify() {
    let fixture = Fixture::new(Product::CodexSubscription, false).await;
    let owner = QuotaOwnerId::new("quota-b").unwrap();
    fixture
        .ledger
        .put_quota_policy(QuotaPolicy {
            owner: owner.clone(),
            max_concurrency: 1,
            unknown: UnknownCapacityPolicy::Reject,
        })
        .await
        .unwrap();
    fixture
        .ledger
        .put_account(Account {
            id: AccountId::new("account-b").unwrap(),
            product: Product::CodexSubscription,
            quota_owner: owner.clone(),
            pools: BTreeSet::from([PoolId::new("pool-b").unwrap()]),
            credential: CredentialRef {
                id: CredentialId::new("key-b").unwrap(),
                generation: 1,
            },
            models: BTreeSet::from(["model-a".into()]),
            enabled: true,
        })
        .await
        .unwrap();
    fixture
        .ledger
        .observe(UsageObservation {
            owner,
            observed_at: 1,
            valid_until: 1000,
            status: CapacityStatus::Available,
            windows: vec![],
            balances: vec![],
            source: "synthetic".into(),
            provider_available: None,
        })
        .await
        .unwrap();
    let body = json!({"model":"model-a", "stream":true, "input":"hello"});
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-ab-token",
            &[("thread-id", "t-ambiguous")],
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(value(response).await["error"]["code"], "invalid_input");
    for (pool, account) in [("pool-b", "account-b"), ("pool-a", "account-a")] {
        let response = fixture
            .app
            .clone()
            .oneshot(pool_request(
                &fixture,
                "/v1/responses",
                "caller-ab-token",
                &[
                    ("thread-id", &format!("t-{pool}")),
                    ("x-poolparty-pool", pool),
                ],
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(drain(response).await["x-poolparty-account"], account);
    }
    // A single-pool grant needs no pin even though the inventory has two pools.
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &[("thread-id", "t-single")],
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(drain(response).await["x-poolparty-account"], "account-a");
}

#[tokio::test]
async fn drop_in_messages_route_derives_the_session_from_metadata() {
    let fixture = Fixture::new(Product::GlmCoding, false).await;
    let body = json!({
        "model":"model-a", "stream":true, "messages":[],
        "metadata":{"user_id":"user_abc_account_def_session_0192bbbb-2222-7ccc-8ddd-eeeeeeeeeeee"}
    });
    let mut bindings = BTreeSet::new();
    for _ in 0..2 {
        let response = fixture
            .app
            .clone()
            .oneshot(pool_request(
                &fixture,
                "/v1/messages",
                "caller-a-token",
                &[],
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let headers = drain(response).await;
        assert_eq!(headers["x-poolparty-account"], "account-a");
        bindings.insert(headers["x-poolparty-binding"].to_str().unwrap().to_owned());
    }
    assert_eq!(bindings.len(), 1);
    let record: Binding = serde_json::from_value(
        value(
            fixture
                .request(
                    "GET",
                    &format!("/api/v1/sessions/{}", bindings.iter().next().unwrap()),
                    Some("caller-a-token"),
                    Value::Null,
                )
                .await,
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        record.intent.session.as_str(),
        "native-0192bbbb-2222-7ccc-8ddd-eeeeeeeeeeee"
    );
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn wildcard_accounts_pass_any_model_through_and_explicit_pins_still_bind() {
    let fixture = Fixture::new(Product::CodexSubscription, false).await;
    let mut account = fixture
        .ledger
        .accounts(&principal("caller-a", &["pool-a"]))
        .await
        .unwrap()
        .remove(0);
    account.models.insert("*".into());
    fixture.ledger.put_account(account).await.unwrap();
    let thread = [("thread-id", "t-any")];
    let mut bindings = BTreeSet::new();
    for model in ["model-a", "model-b", "model-c"] {
        let response = fixture
            .app
            .clone()
            .oneshot(pool_request(
                &fixture,
                "/v1/responses",
                "caller-a-token",
                &thread,
                json!({"model":model, "stream":true, "input":"hello"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        bindings.insert(
            drain(response).await["x-poolparty-binding"]
                .to_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(bindings.len(), 1);

    // The explicit API without a model is affinity-only too; with a model it is a hard pin.
    let loose: Binding = serde_json::from_value(
        value(
            fixture
                .request(
                    "POST",
                    "/api/v1/sessions",
                    Some("caller-a-token"),
                    json!({"session":"loose", "pool":"pool-a", "product":"codex_subscription"}),
                )
                .await,
        )
        .await,
    )
    .unwrap();
    assert_eq!(loose.intent.model, None);
    let pinned = fixture.create().await;
    let response = fixture
        .request(
            "POST",
            &fixture.route(&pinned.id),
            Some("caller-a-token"),
            json!({"model":"model-b", "stream":true, "input":"hello"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(value(response).await["error"]["code"], "intent_conflict");
    assert_eq!(fixture.transport.calls.load(Ordering::SeqCst), 3);
}

fn admin_grants() -> BearerGrants {
    let mut operator = principal("operator", &["pool-a"]);
    operator.admin = true;
    BearerGrants::new(vec![
        (
            SecretValue::new("caller-a-token".into()),
            principal("caller-a", &["pool-a"]),
        ),
        (SecretValue::new("operator-token".into()), operator),
    ])
    .unwrap()
}

#[tokio::test]
async fn operators_list_and_resolve_uncertain_attempts_over_http() {
    let mut fixture = Fixture::new(Product::CodexSubscription, true).await;
    fixture.app = app(
        Arc::new(Router::new(
            fixture.ledger.clone(),
            fixture.transport.clone(),
            Arc::new(Credentials),
            Arc::new(FixedClock),
        )),
        admin_grants(),
    );
    let binding = fixture.create().await;
    // A failing stream leaves the attempt uncertain and fences the binding.
    let response = fixture
        .request(
            "POST",
            &fixture.route(&binding.id),
            Some("caller-a-token"),
            model_body(),
        )
        .await;
    let attempt = response.headers()["x-poolparty-attempt-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let _ = to_bytes(response.into_body(), 4096).await;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let body = value(
                fixture
                    .request(
                        "GET",
                        "/api/v1/attempts?state=uncertain",
                        Some("operator-token"),
                        Value::Null,
                    )
                    .await,
            )
            .await;
            if body["attempts"]
                .as_array()
                .is_some_and(|list| list.len() == 1)
            {
                assert_eq!(body["attempts"][0]["id"], attempt.as_str());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let sessions = value(
        fixture
            .request(
                "GET",
                "/api/v1/sessions?open=true",
                Some("operator-token"),
                Value::Null,
            )
            .await,
    )
    .await;
    assert_eq!(sessions["sessions"][0]["id"], binding.id.as_str());

    // The owner cannot resolve; the operator can, once, with a rationale.
    let path = format!("/api/v1/attempts/{attempt}/resolve");
    let body =
        json!({"outcome":"rejected","rationale":"upstream request log shows an early abort"});
    let response = fixture
        .request("POST", &path, Some("caller-a-token"), body.clone())
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = fixture
        .request(
            "POST",
            &path,
            Some("operator-token"),
            json!({"outcome":"rejected"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = fixture
        .request("POST", &path, Some("operator-token"), body.clone())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let resolved = value(response).await;
    assert_eq!(resolved["state"], "rejected");
    assert_eq!(resolved["resolution"]["principal"], "operator");
    let response = fixture
        .request("POST", &path, Some("operator-token"), body)
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // The thread is usable again for its owner.
    let response = fixture
        .request(
            "POST",
            &fixture.route(&binding.id),
            Some("caller-a-token"),
            model_body(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn subagent_threads_follow_their_parent_account() {
    let fixture = Fixture::new(Product::CodexSubscription, false).await;
    let body = json!({"model":"model-a", "stream":true, "input":"hello"});
    let parent = "0192cccc-3333-7ddd-8eee-ffffffffffff";
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &[("thread-id", parent)],
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    drain(response).await;
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &[
                ("thread-id", "child-1"),
                ("x-codex-parent-thread-id", parent),
            ],
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let child = drain(response).await["x-poolparty-binding"]
        .to_str()
        .unwrap()
        .to_owned();
    let record: Binding = serde_json::from_value(
        value(
            fixture
                .request(
                    "GET",
                    &format!("/api/v1/sessions/{child}"),
                    Some("caller-a-token"),
                    Value::Null,
                )
                .await,
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        record.intent.account.as_ref().unwrap().as_str(),
        "account-a"
    );
    assert_eq!(record.intent.session.as_str(), "native-child-1");
    // An unknown parent is ignored rather than refused.
    let response = fixture
        .app
        .clone()
        .oneshot(pool_request(
            &fixture,
            "/v1/responses",
            "caller-a-token",
            &[
                ("thread-id", "child-2"),
                ("x-codex-parent-thread-id", "never-bound"),
            ],
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    drain(response).await;
}
