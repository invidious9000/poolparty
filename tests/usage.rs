use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router, body::Body, extract::Request, http::StatusCode, response::Response, routing::get,
};
use poolparty::{
    domain::*,
    ports::{SecretValue, UsageCollector},
    usage::{HttpUsageCollector, UsageEndpoint, parse_usage},
};
use serde_json::json;
use tokio::{sync::Mutex, task::JoinHandle};

fn parse(product: Product, value: serde_json::Value) -> UsageObservation {
    parse_usage(
        product,
        QuotaOwnerId::new("owner-a").unwrap(),
        &serde_json::to_vec(&value).unwrap(),
        1000,
    )
    .unwrap()
}

#[test]
fn codex_preserves_windows_features_resets_and_fractional_percentages() {
    let observation = parse(
        Product::CodexSubscription,
        json!({
            "rate_limit": {"allowed":true,"limit_reached":false,
                "primary_window":{"used_percent":"99.9999999999999999999999","limit_window_seconds":18000,"reset_at":200},
                "secondary_window":{"used_percent":12,"limit_window_seconds":604800,"reset_at":300}},
            "additional_rate_limits":[{"metered_feature":"feature-a","rate_limit":{"allowed":true,"limit_reached":false,
                "primary_window":{"used_percent":25,"limit_window_seconds":3600,"reset_at":100}}}]
        }),
    );
    assert_eq!(observation.status, CapacityStatus::Available);
    assert_eq!(observation.windows.len(), 3);
    assert_eq!(
        observation.windows[0].used_percent.as_deref(),
        Some("99.9999999999999999999999")
    );
    assert_eq!(observation.windows[0].used, None);
    assert_eq!(observation.windows[0].limit, None);
    assert_eq!(observation.windows[0].resets_at, Some(200_000));
    assert_eq!(observation.windows[1].window_seconds, Some(604800));
    assert!(observation.windows[2].key.contains("feature-a"));
    assert_eq!(observation.provider_available, Some(true));
    assert_eq!(observation.observed_at, 1000);
    assert_eq!(observation.valid_until, 61_000);
    assert_eq!(observation.owner.as_str(), "owner-a");
}

#[test]
fn numeric_percentages_do_not_round_across_exhaustion_boundary() {
    let body = br#"{"rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":99.999999999999999999999999}}}"#;
    let observation = parse_usage(
        Product::CodexSubscription,
        QuotaOwnerId::new("owner-a").unwrap(),
        body,
        0,
    )
    .unwrap();
    assert_eq!(observation.status, CapacityStatus::Available);
    assert_eq!(
        observation.windows[0].used_percent.as_deref(),
        Some("99.999999999999999999999999")
    );
}

#[test]
fn auxiliary_exhaustion_does_not_become_account_wide_generation_exhaustion() {
    let observation = parse(
        Product::CodexSubscription,
        json!({
            "rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":10}},
            "additional_rate_limits":[{"metered_feature":"code_review","rate_limit":{
                "primary_window":{"used_percent":100},"secondary_window":null}}]
        }),
    );
    assert_eq!(observation.status, CapacityStatus::Available);
    assert_eq!(observation.windows[1].used_percent.as_deref(), Some("100"));
    assert!(observation.windows[1].key.contains("code_review"));
    let observation = parse(
        Product::GlmCoding,
        json!({"limits":[
            {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":10},
            {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":20},
            {"type":"TIME_LIMIT","unit":5,"number":1,"percentage":100,"usage":100,"currentValue":100}
        ]}),
    );
    assert_eq!(observation.status, CapacityStatus::Available);
    assert_eq!(observation.windows[2].used_percent.as_deref(), Some("100"));
    assert_eq!(
        parse(
            Product::GlmCoding,
            json!({"limits":[
                {"type":"TIME_LIMIT","unit":5,"number":1,"percentage":100}
            ]})
        )
        .status,
        CapacityStatus::Unknown
    );
}

#[test]
fn codex_model_restrictions_remain_unknown_until_model_scoped_admission() {
    let observation = parse(
        Product::CodexSubscription,
        json!({
            "rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":10}},
            "model_usage":{"synthetic-model":{"available":false,"credits_would_enable":true}}
        }),
    );
    assert_eq!(observation.status, CapacityStatus::Unknown);
    assert_eq!(observation.provider_available, Some(true));
}

#[test]
fn codex_missing_fields_stay_unknown_and_exhaustion_dominates_partial_unknown() {
    for value in [
        json!({}),
        json!({"rate_limit":{"allowed":true,"limit_reached":false}}),
        json!({"rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{}}}),
        json!({"rate_limit":{"primary_window":{"used_percent":10}}}),
    ] {
        assert_eq!(
            parse(Product::CodexSubscription, value).status,
            CapacityStatus::Unknown
        );
    }
    let observation = parse(
        Product::CodexSubscription,
        json!({"rate_limit":{"allowed":true,"limit_reached":false,
        "primary_window":{},"secondary_window":{"used_percent":100}}}),
    );
    assert_eq!(observation.status, CapacityStatus::Exhausted);
    assert_eq!(observation.windows[0].used_percent, None);
    assert_eq!(observation.windows[0].resets_at, None);
    assert_eq!(
        parse(
            Product::CodexSubscription,
            json!({"rate_limit":{"limit_reached":true}})
        )
        .status,
        CapacityStatus::Exhausted
    );
    assert_eq!(
        parse(
            Product::CodexSubscription,
            json!({"spend_control":{"reached":true}})
        )
        .status,
        CapacityStatus::Exhausted
    );
}

#[test]
fn malformed_percentages_and_timestamps_never_become_zero() {
    for percent in [
        json!(-1),
        json!("not-a-number"),
        json!(null),
        json!("1e2"),
        json!("NaN"),
    ] {
        let observation = parse(
            Product::CodexSubscription,
            json!({"rate_limit":{"allowed":true,"limit_reached":false,
            "primary_window":{"used_percent":percent,"reset_at":i64::MAX,"limit_window_seconds":-1}}}),
        );
        assert_eq!(observation.status, CapacityStatus::Unknown);
        assert_eq!(observation.windows[0].used_percent, None);
        assert_eq!(observation.windows[0].resets_at, None);
        assert_eq!(observation.windows[0].window_seconds, None);
    }
}

#[test]
fn glm_keeps_five_hour_weekly_and_other_rows_without_inventing_scopes() {
    let observation = parse(
        Product::GlmCoding,
        json!({"code":200,"success":true,"data":{"limits":[
            {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":20,"nextResetTime":200_000},
            {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":50},
            {"type":"TIME_LIMIT","unit":5,"number":1,"currentValue":3,"usage":100},
            {"type":"FUTURE_LIMIT","unit":99,"number":2,"percentage":25}
        ]}}),
    );
    assert_eq!(observation.windows.len(), 4);
    assert_eq!(observation.windows[0].window_seconds, Some(18000));
    assert_eq!(observation.windows[1].window_seconds, Some(604800));
    assert_eq!(observation.windows[0].resets_at, Some(200_000));
    assert_eq!(observation.windows[1].resets_at, None);
    assert_eq!(observation.windows[2].used, Some(3));
    assert_eq!(observation.windows[2].limit, Some(100));
    assert!(
        observation.windows[3]
            .key
            .contains("FUTURE_LIMIT/unit-99/number-2")
    );
    assert_eq!(observation.status, CapacityStatus::Unknown);
}

#[test]
fn glm_exhaustion_beats_incomplete_rows_and_zero_limit_is_unknown() {
    let observation = parse(
        Product::GlmCoding,
        json!({"data":{"limits":[
            {"type":"TOKENS_LIMIT","unit":3,"number":5},
            {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":"100.0"}
        ]}}),
    );
    assert_eq!(observation.status, CapacityStatus::Exhausted);
    assert_eq!(observation.windows[0].used_percent, None);
    assert_eq!(parse(Product::GlmCoding, json!({"limits":[
        {"type":"TOKENS_LIMIT","unit":3,"number":5,"usage":0,"currentValue":0,"percentage":0}
    ]})).status, CapacityStatus::Unknown);
    assert_eq!(
        parse(
            Product::GlmCoding,
            json!({"limits":[
                {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":10},
                {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":20}
            ]})
        )
        .status,
        CapacityStatus::Available
    );
}

#[test]
fn deepseek_retains_all_currencies_components_and_exact_decimals() {
    let observation = parse(
        Product::DeepseekPayg,
        json!({"is_available":true,"balance_infos":[
            {"currency":"USD","total_balance":"0.0000000000000000000001","granted_balance":"0.00","topped_up_balance":"0.0000000000000000000001"},
            {"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"}
        ]}),
    );
    assert_eq!(observation.balances.len(), 6);
    assert_eq!(observation.balances[0].currency, "USD");
    assert_eq!(observation.balances[0].decimal, "0.0000000000000000000001");
    assert_eq!(observation.balances[0].kind.as_deref(), Some("total"));
    assert_eq!(observation.balances[1].kind.as_deref(), Some("granted"));
    assert_eq!(observation.balances[5].currency, "CNY");
    assert_eq!(observation.balances[5].kind.as_deref(), Some("topped_up"));
    assert_eq!(observation.provider_available, Some(true));
    assert_eq!(observation.status, CapacityStatus::Unknown);
    assert!(observation.windows.is_empty());
    assert_eq!(
        parse(Product::DeepseekPayg, json!({"is_available":false})).status,
        CapacityStatus::Exhausted
    );
    assert_eq!(
        parse(Product::DeepseekPayg, json!({})).status,
        CapacityStatus::Unknown
    );
}

#[test]
fn parsing_failures_are_bounded_and_sanitized() {
    for (product, body) in [
        (
            Product::GlmCoding,
            br#"{"success":false,"code":401,"msg":"synthetic-sensitive-body"}"#.to_vec(),
        ),
        (Product::GlmCoding, b"synthetic-sensitive-body".to_vec()),
        (Product::CodexSubscription, b"[]".to_vec()),
        (Product::CodexSubscription, vec![b' '; 256 * 1024 + 1]),
        (
            Product::DeepseekPayg,
            br#"{"balance_infos":[{"currency":"USD","total_balance":1.0}]}"#.to_vec(),
        ),
        (
            Product::DeepseekPayg,
            br#"{"balance_infos":[{"currency":"USD","total_balance":"NaN"}]}"#.to_vec(),
        ),
    ] {
        let error =
            parse_usage(product, QuotaOwnerId::new("owner-a").unwrap(), &body, 0).unwrap_err();
        assert_eq!(error.code, ErrorCode::UpstreamUnavailable);
        assert!(!error.to_string().contains("synthetic-sensitive-body"));
    }
    assert_eq!(
        parse_usage(
            Product::KimiCoding,
            QuotaOwnerId::new("owner-a").unwrap(),
            b"{}",
            0
        )
        .unwrap_err()
        .code,
        ErrorCode::Unsupported
    );
    assert!(
        parse_usage(
            Product::DeepseekPayg,
            QuotaOwnerId::new("owner-a").unwrap(),
            b"{}",
            i64::MAX
        )
        .is_err()
    );
}

struct Origin {
    url: String,
    calls: Arc<AtomicUsize>,
    headers: Arc<Mutex<Vec<axum::http::HeaderMap>>>,
    task: JoinHandle<()>,
}
impl Drop for Origin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn origin(status: StatusCode, body: Vec<u8>) -> Origin {
    let calls = Arc::new(AtomicUsize::new(0));
    let headers = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route(
        "/usage",
        get({
            let calls = calls.clone();
            let headers = headers.clone();
            move |request: Request| {
                let calls = calls.clone();
                let headers = headers.clone();
                let body = body.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    headers.lock().await.push(request.headers().clone());
                    Response::builder()
                        .status(status)
                        .header("location", "/usage")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Origin {
        url: format!("http://{address}/usage"),
        calls,
        headers,
        task,
    }
}

fn account(product: Product) -> Account {
    Account {
        id: AccountId::new("account-a").unwrap(),
        product,
        quota_owner: QuotaOwnerId::new("owner-a").unwrap(),
        pools: BTreeSet::new(),
        credential: CredentialRef {
            id: CredentialId::new("credential-a").unwrap(),
            generation: 1,
        },
        models: BTreeSet::new(),
        enabled: true,
    }
}

fn collector(server: &Origin, product: Product) -> HttpUsageCollector {
    HttpUsageCollector::new(
        vec![UsageEndpoint {
            product,
            url: server.url.clone(),
        }],
        true,
    )
    .unwrap()
}

#[tokio::test]
async fn collectors_use_product_specific_auth_and_no_inference_requests() {
    for (product, secret, expected) in [
        (
            Product::CodexSubscription,
            r#"{"tokens":{"access_token":"synthetic-token","account_id":"synthetic-account"}}"#,
            "Bearer synthetic-token",
        ),
        (Product::GlmCoding, "synthetic-key", "synthetic-key"),
        (
            Product::DeepseekPayg,
            "synthetic-key",
            "Bearer synthetic-key",
        ),
    ] {
        let server = origin(StatusCode::OK, b"{}".to_vec()).await;
        let observation = collector(&server, product)
            .collect(&account(product), &SecretValue::new(secret.into()), 1000)
            .await
            .unwrap();
        assert_eq!(observation.status, CapacityStatus::Unknown);
        let headers = server.headers.lock().await;
        assert_eq!(headers[0]["authorization"], expected);
        assert_eq!(headers[0]["accept"], "application/json");
        assert!(
            headers[0]["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("poolparty/")
        );
        if product == Product::CodexSubscription {
            assert_eq!(headers[0]["chatgpt-account-id"], "synthetic-account");
        } else {
            assert!(headers[0].get("chatgpt-account-id").is_none());
        }
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn authentication_denial_is_an_observation_but_throttling_is_not_exhaustion() {
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::TEMPORARY_REDIRECT,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let server = origin(status, b"synthetic-sensitive-body".to_vec()).await;
        let result = collector(&server, Product::GlmCoding)
            .collect(
                &account(Product::GlmCoding),
                &SecretValue::new("synthetic-key".into()),
                1000,
            )
            .await;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            let observation = result.unwrap();
            assert_eq!(observation.status, CapacityStatus::ReauthenticationRequired);
            assert_eq!(observation.valid_until, 61_000);
            assert!(observation.windows.is_empty());
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, ErrorCode::UpstreamUnavailable);
            assert!(!error.to_string().contains("synthetic-sensitive-body"));
        }
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn unsupported_products_bad_credentials_and_untrusted_endpoints_fail_locally() {
    let server = origin(StatusCode::OK, b"{}".to_vec()).await;
    assert_eq!(
        collector(&server, Product::KimiCoding)
            .collect(
                &account(Product::KimiCoding),
                &SecretValue::new("synthetic-key".into()),
                0
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    assert_eq!(
        collector(&server, Product::CodexSubscription)
            .collect(
                &account(Product::CodexSubscription),
                &SecretValue::new("malformed".into()),
                0
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::CredentialUnavailable
    );
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    for url in [
        "http://example.com/usage",
        "http://localhost/usage",
        "https://user:password@example.com/usage",
        "https://example.com/usage?key=secret",
    ] {
        assert!(
            HttpUsageCollector::new(
                vec![UsageEndpoint {
                    product: Product::GlmCoding,
                    url: url.into()
                }],
                true
            )
            .is_err()
        );
    }
    assert!(
        HttpUsageCollector::new(
            vec![UsageEndpoint {
                product: Product::GlmCoding,
                url: server.url.clone()
            }],
            false
        )
        .is_err()
    );
}

#[tokio::test]
async fn oversized_http_payload_is_rejected() {
    let server = origin(StatusCode::OK, vec![b' '; 256 * 1024 + 1]).await;
    let error = collector(&server, Product::GlmCoding)
        .collect(
            &account(Product::GlmCoding),
            &SecretValue::new("synthetic-key".into()),
            0,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UpstreamUnavailable);
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
