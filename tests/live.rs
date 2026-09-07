use poolparty::{
    domain::{CredentialId, ErrorCode},
    live::{LiveConfig, LiveMode, run},
    ports::SecretValue,
};
use serde_json::{Value, json};

fn config(state_dir: &std::path::Path) -> Value {
    json!({
        "state_dir":state_dir,
        "op_executable":"/nonexistent/poolparty-synthetic-op",
        "credentials":[
            {"credential":"credential-a","vault":"vault-a","item":"item-a","field":"auth-json"},
            {"credential":"credential-b","vault":"vault-a","item":"item-b","field":"auth-json"}
        ],
        "oauth":{"endpoint":"https://example.com/oauth/token","client_id":"synthetic-client"},
        "accounts":[
            {"id":"account-a","product":"codex_subscription","quota_owner":"quota-a","pool":"pool-a",
             "credential":"credential-a","model":"model-a","expected_account_id":"workspace-a",
             "max_concurrency":1,"unknown_capacity":"reject"},
            {"id":"account-b","product":"codex_subscription","quota_owner":"quota-a","pool":"pool-b",
             "credential":"credential-a","model":"model-a","expected_account_id":"workspace-a",
             "max_concurrency":1,"unknown_capacity":"reject"}
        ]
    })
}

async fn rejected_without_state(value: Value, state_dir: &std::path::Path, mode: LiveMode) {
    let config: LiveConfig = serde_json::from_value(value).unwrap();
    let error = run(
        config,
        SecretValue::new("synthetic-service-token".into()),
        mode,
    )
    .await
    .err()
    .expect("invalid inventory must fail");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(
        !state_dir.exists(),
        "preflight must fail before state acquisition or credential access"
    );
}

#[tokio::test]
async fn conflicting_credential_aliases_fail_before_state_or_external_access() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (field, replacement) in [
        ("product", json!("glm_coding")),
        ("expected_account_id", json!("workspace-b")),
        ("quota_owner", json!("quota-b")),
    ] {
        let state_dir = root.join(field);
        let mut value = config(&state_dir);
        value["accounts"][1][field] = replacement;
        rejected_without_state(
            value,
            &state_dir,
            LiveMode::Refresh(CredentialId::new("credential-a").unwrap()),
        )
        .await;
    }
}

#[tokio::test]
async fn shared_quota_policies_cannot_conflict_even_with_distinct_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (field, replacement) in [
        ("max_concurrency", json!(2)),
        ("unknown_capacity", json!("allow_under_local_cap")),
    ] {
        let state_dir = root.join(field);
        let mut value = config(&state_dir);
        value["accounts"][1]["credential"] = json!("credential-b");
        value["accounts"][1][field] = replacement;
        rejected_without_state(value, &state_dir, LiveMode::Check).await;
    }
}

#[tokio::test]
async fn invalid_basic_inventory_and_refresh_target_fail_before_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut variants = vec![];
    for field in [
        "id",
        "credential",
        "model",
        "max_concurrency",
        "expected_account_id",
    ] {
        let state_dir = root.join(field);
        let mut value = config(&state_dir);
        match field {
            "id" => value["accounts"][1][field] = json!("account-a"),
            "credential" => value["accounts"][1][field] = json!("unmapped"),
            "model" => value["accounts"][1][field] = json!(" "),
            "max_concurrency" => value["accounts"][1][field] = json!(0),
            "expected_account_id" => value["accounts"][1][field] = Value::Null,
            _ => unreachable!(),
        }
        variants.push((state_dir, value));
    }
    for (state_dir, value) in variants {
        rejected_without_state(value, &state_dir, LiveMode::Probe).await;
    }
    let state_dir = root.join("target");
    rejected_without_state(
        config(&state_dir),
        &state_dir,
        LiveMode::Refresh(CredentialId::new("unknown").unwrap()),
    )
    .await;
    let state_dir = root.join("empty");
    let mut value = config(&state_dir);
    value["accounts"] = json!([]);
    rejected_without_state(value, &state_dir, LiveMode::Check).await;
}
