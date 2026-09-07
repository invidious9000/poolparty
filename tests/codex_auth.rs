use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use poolparty::{
    codex_auth::{CodexAuth, CodexRefresher},
    domain::ErrorCode,
    ports::SecretValue,
};
use serde_json::{Value, json};

fn jwt(claims: Value) -> String {
    format!(
        "e30.{}.synthetic-signature",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}
fn claims(exp: Value) -> Value {
    json!({"sub":"subject-a", "exp":exp, "https://api.openai.com/auth":{"chatgpt_account_id":"workspace-a"}})
}
fn bundle() -> Value {
    json!({
        "auth_mode":"chatgpt", "OPENAI_API_KEY":null,
        "tokens":{
            "account_id":"workspace-a", "access_token":jwt(claims(json!(200))),
            "id_token":jwt(claims(json!(500))), "refresh_token":"synthetic-refresh-a",
            "future_token_metadata":{"keep":true}
        },
        "last_refresh":"1970-01-01T00:00:00Z", "future_root_metadata":[1,2,3]
    })
}
fn secret(value: Value) -> SecretValue {
    SecretValue::new(value.to_string())
}

struct MockState {
    calls: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    status: StatusCode,
    body: String,
    redirect: bool,
}
struct Mock {
    url: String,
    state: Arc<MockState>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    async fn new(status: StatusCode, body: String, redirect: bool) -> Self {
        let state = Arc::new(MockState {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
            status,
            body,
            redirect,
        });
        let app = Router::new()
            .route("/oauth/token", post(exchange))
            .route("/redirect-target", post(exchange))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/oauth/token", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, state, task }
    }
    fn refresher(&self) -> CodexRefresher {
        CodexRefresher::new(
            self.url.clone(),
            "synthetic-client-registration".into(),
            true,
        )
        .unwrap()
    }
}
async fn exchange(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    assert!(!headers.contains_key("authorization"));
    assert_eq!(headers["content-type"], "application/json");
    state
        .requests
        .lock()
        .unwrap()
        .push(serde_json::from_slice(&body).unwrap());
    let mut response = (state.status, state.body.clone()).into_response();
    if state.redirect {
        response
            .headers_mut()
            .insert("location", "/redirect-target".parse().unwrap());
    }
    response
}

#[test]
fn expiry_and_skew_are_conservative_and_account_is_not_subject() {
    let auth = CodexAuth::parse(&secret(bundle())).unwrap();
    assert_eq!(auth.account_id(), "workspace-a");
    assert!(!auth.needs_refresh(100_000, 99_999).unwrap());
    assert!(auth.needs_refresh(100_000, 100_000).unwrap());
    assert!(auth.needs_refresh(200_000, 0).unwrap());
    assert!(auth.needs_refresh(i64::MAX, 1).unwrap());
    assert!(auth.needs_refresh(-1, 0).is_err());
    assert!(auth.needs_refresh(0, -1).is_err());
    for expiry in [
        Value::Null,
        json!("200"),
        json!(-1),
        json!(i64::MAX),
        json!(200.5),
    ] {
        let mut value = bundle();
        value["tokens"]["access_token"] = json!(jwt(claims(expiry)));
        assert!(
            CodexAuth::parse(&secret(value))
                .unwrap()
                .needs_refresh(0, 0)
                .unwrap()
        );
    }
    let mut value = bundle();
    value["tokens"]["access_token"] = json!("opaque-access-token");
    assert!(
        CodexAuth::parse(&secret(value))
            .unwrap()
            .needs_refresh(0, 0)
            .unwrap()
    );
}

#[test]
fn malformed_credentials_and_conflicting_claims_fail_without_disclosure() {
    for path in ["account_id", "access_token", "refresh_token", "id_token"] {
        for malformed in [
            Value::Null,
            json!(""),
            json!(1),
            json!("token with whitespace"),
        ] {
            let mut value = bundle();
            value["tokens"][path] = malformed;
            let error = CodexAuth::parse(&secret(value)).unwrap_err();
            assert!(!format!("{error:?}").contains("synthetic-refresh-a"));
        }
    }
    for malformed in ["bad.jwt", "bad.%%%.signature", "bad.e30."] {
        let mut value = bundle();
        value["tokens"]["access_token"] = json!(malformed);
        assert!(CodexAuth::parse(&secret(value)).is_err());
    }
    let mut value = bundle();
    value["tokens"]["access_token"] = json!(jwt(json!({"sub":"subject-b"})));
    assert_eq!(
        CodexAuth::parse(&secret(value)).unwrap_err().code,
        ErrorCode::ReauthenticationRequired
    );
    let mut value = bundle();
    value["tokens"]["id_token"] = json!(jwt(json!({"chatgpt_account_id":"workspace-b"})));
    assert!(CodexAuth::parse(&secret(value)).is_err());
    assert_eq!(
        format!("{:?}", CodexAuth::parse(&secret(bundle())).unwrap()),
        "CodexAuth([REDACTED])"
    );
}

#[test]
fn endpoint_validation_requires_https_or_explicit_literal_loopback() {
    for endpoint in [
        "http://example.com/oauth/token",
        "http://localhost/oauth/token",
        "https://user:password@example.com/oauth/token",
        "https://example.com/oauth/token?secret=value",
        "https://example.com/oauth/token#fragment",
        "file:///tmp/token",
    ] {
        assert!(CodexRefresher::new(endpoint.into(), "client-a".into(), true).is_err());
    }
    assert!(
        CodexRefresher::new(
            "http://127.0.0.1/oauth/token".into(),
            "client-a".into(),
            false
        )
        .is_err()
    );
    assert!(
        CodexRefresher::new("https://example.com/oauth/token".into(), "".into(), false).is_err()
    );
    let refresher = CodexRefresher::new(
        "https://example.com/oauth/token".into(),
        "client-a".into(),
        false,
    )
    .unwrap();
    assert_eq!(format!("{refresher:?}"), "CodexRefresher([REDACTED])");
}

#[tokio::test]
async fn refresh_merges_only_tokens_and_rfc3339_timestamp() {
    let new_access = jwt(claims(json!(900)));
    let new_id = jwt(claims(json!(1000)));
    let mock = Mock::new(
        StatusCode::OK,
        json!({
            "access_token":new_access, "id_token":new_id, "refresh_token":"synthetic-refresh-b",
            "account_id":"must-not-overwrite", "unexpected_response_field":"ignored"
        })
        .to_string(),
        false,
    )
    .await;
    let original = bundle();
    let updated = mock
        .refresher()
        .refresh(&secret(original.clone()), 123_456)
        .await
        .unwrap();
    let actual: Value = serde_json::from_str(updated.expose()).unwrap();
    let mut expected = original;
    expected["tokens"]["access_token"] = json!(new_access);
    expected["tokens"]["id_token"] = json!(new_id);
    expected["tokens"]["refresh_token"] = json!("synthetic-refresh-b");
    expected["last_refresh"] = json!("1970-01-01T00:02:03.456Z");
    assert_eq!(actual, expected);
    assert_eq!(mock.state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        mock.state.requests.lock().unwrap()[0],
        json!({
            "grant_type":"refresh_token", "client_id":"synthetic-client-registration", "refresh_token":"synthetic-refresh-a"
        })
    );
}

#[tokio::test]
async fn omitted_rotating_fields_are_preserved() {
    let mock = Mock::new(
        StatusCode::OK,
        json!({"access_token":"replacement-opaque-access"}).to_string(),
        false,
    )
    .await;
    let original = bundle();
    let updated = mock
        .refresher()
        .refresh(&secret(original.clone()), 0)
        .await
        .unwrap();
    let actual: Value = serde_json::from_str(updated.expose()).unwrap();
    assert_eq!(actual["tokens"]["id_token"], original["tokens"]["id_token"]);
    assert_eq!(
        actual["tokens"]["refresh_token"],
        original["tokens"]["refresh_token"]
    );
    assert_eq!(actual["tokens"]["account_id"], "workspace-a");
    assert!(
        CodexAuth::parse(&updated)
            .unwrap()
            .needs_refresh(0, 0)
            .unwrap()
    );
}

#[tokio::test]
async fn refresh_rejects_changed_workspace_or_subject_in_either_token() {
    for field in ["access_token", "id_token"] {
        for bad_claims in [
            json!({"https://api.openai.com/auth":{"chatgpt_account_id":"workspace-b"}, "sub":"subject-a"}),
            json!({"chatgpt_account_id":"workspace-b", "sub":"subject-a"}),
            json!({"sub":"subject-b"}),
            json!({"sub":12}),
            json!({"https://api.openai.com/auth":{"chatgpt_account_id":null}}),
        ] {
            let mut response = json!({"access_token":jwt(claims(json!(900)))});
            response[field] = json!(jwt(bad_claims));
            let mock = Mock::new(StatusCode::OK, response.to_string(), false).await;
            let error = mock
                .refresher()
                .refresh(&secret(bundle()), 0)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::ReauthenticationRequired);
            assert_eq!(mock.state.calls.load(Ordering::SeqCst), 1);
            assert!(!format!("{error:?}").contains("workspace-b"));
        }
    }
}

#[tokio::test]
async fn malformed_partial_responses_never_silently_preserve_supplied_fields() {
    let mut cases = vec![
        json!({}),
        json!([]),
        json!({"access_token":null}),
        json!({"access_token":""}),
    ];
    for field in ["id_token", "refresh_token", "access_token"] {
        for value in [Value::Null, json!(""), json!(22), json!("has space")] {
            let mut response = json!({"access_token":"replacement-access"});
            response[field] = value;
            cases.push(response);
        }
    }
    cases.push(json!({"access_token":"bad.%%%.jwt"}));
    cases
        .push(json!({"access_token":"replacement-access", "id_token":"opaque-is-not-an-id-token"}));
    for response in cases {
        let mock = Mock::new(StatusCode::OK, response.to_string(), false).await;
        assert!(
            mock.refresher()
                .refresh(&secret(bundle()), 0)
                .await
                .is_err()
        );
        assert_eq!(mock.state.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn server_failures_redirects_and_oversized_bodies_are_not_retried_or_disclosed() {
    for (status, response, redirect) in [
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "synthetic-private-upstream-body".into(),
            false,
        ),
        (
            StatusCode::BAD_REQUEST,
            json!({"error":"invalid_grant", "error_description":"synthetic-private-upstream-body"})
                .to_string(),
            false,
        ),
        (StatusCode::TEMPORARY_REDIRECT, "redirect".into(), true),
        (StatusCode::OK, "x".repeat(256 * 1024 + 1), false),
        (
            StatusCode::OK,
            "synthetic-private-invalid-json".into(),
            false,
        ),
    ] {
        let mock = Mock::new(status, response, redirect).await;
        let error = mock
            .refresher()
            .refresh(&secret(bundle()), 0)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ReauthenticationRequired);
        assert_eq!(mock.state.calls.load(Ordering::SeqCst), 1);
        for needle in ["synthetic-private", "synthetic-refresh-a", "127.0.0.1"] {
            assert!(!format!("{error:?} {error}").contains(needle));
        }
    }
}

#[tokio::test]
async fn invalid_input_fails_before_refresh_dispatch() {
    let mock = Mock::new(StatusCode::OK, "{}".into(), false).await;
    let mut current = bundle();
    current["tokens"]
        .as_object_mut()
        .unwrap()
        .remove("refresh_token");
    assert!(mock.refresher().refresh(&secret(current), 0).await.is_err());
    assert!(
        mock.refresher()
            .refresh(&secret(bundle()), -1)
            .await
            .is_err()
    );
    assert!(
        mock.refresher()
            .refresh(&secret(bundle()), i64::MAX)
            .await
            .is_err()
    );
    assert_eq!(mock.state.calls.load(Ordering::SeqCst), 0);
}
