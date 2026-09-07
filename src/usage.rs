//! Bounded, read-only usage observations. No inference probes or wallet-based
//! spending admission are implemented here. Unknown fields never become zero.
//!
//! Schema/auth evidence (original parser implementation, no source copied):
//! - Codex internal backend model and reader, pinned source observation:
//!   <https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/codex-backend-openapi-models/src/models/rate_limit_status_payload.rs>
//!   <https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/backend-client/src/client.rs>
//! - GLM monitoring reader, pinned official plugin observation:
//!   <https://github.com/zai-org/zai-coding-plugins/blob/0446d0bb0bc537d97d3ab3664c4b8b9c4a0e1254/plugins/glm-plan-usage/skills/usage-query-skill/scripts/query-usage.mjs>
//! - DeepSeek published balance schema:
//!   <https://api-docs.deepseek.com/api/get-user-balance/>
//!
//! Codex/GLM internal schemas remain conformance gates. Kimi collection is
//! explicitly unsupported until its current account-usage contract is qualified.
//! Auxiliary Codex buckets and GLM tool windows are observations, not proof of
//! exhausted generation capacity. Per-model restriction admission remains a gate.
use std::{net::IpAddr, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{
    Client, Url,
    header::{HeaderMap, HeaderValue},
};
use serde_json::Value;

use crate::{
    domain::*,
    ports::{SecretValue, UsageCollector},
};

const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_ROWS: usize = 128;
const TTL_MS: i64 = 60_000;

pub struct UsageEndpoint {
    pub product: Product,
    /// Full GET endpoint from trusted operator configuration.
    pub url: String,
}

pub struct HttpUsageCollector {
    client: Client,
    endpoints: Vec<(Product, Url)>,
}

impl HttpUsageCollector {
    pub fn new(endpoints: Vec<UsageEndpoint>, allow_loopback_http: bool) -> Result<Self> {
        let mut parsed = Vec::new();
        for endpoint in endpoints {
            let url = Url::parse(&endpoint.url).map_err(|_| configuration_error())?;
            let loopback = url
                .host_str()
                .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
            if !(url.scheme() == "https"
                || (allow_loopback_http && url.scheme() == "http" && loopback))
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || parsed
                    .iter()
                    .any(|(product, _)| *product == endpoint.product)
            {
                return Err(configuration_error());
            }
            parsed.push((endpoint.product, url));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("poolparty/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| configuration_error())?;
        Ok(Self {
            client,
            endpoints: parsed,
        })
    }
}

fn configuration_error() -> Error {
    Error::new(
        ErrorCode::InvalidInput,
        "invalid usage endpoint configuration",
    )
}
fn collection_error() -> Error {
    Error::new(ErrorCode::UpstreamUnavailable, "usage collection failed")
}
fn credential_error() -> Error {
    Error::new(
        ErrorCode::CredentialUnavailable,
        "usage credential unavailable",
    )
}
fn unsupported() -> Error {
    Error::new(
        ErrorCode::Unsupported,
        "usage collection unsupported for product",
    )
}

fn sensitive(value: &str) -> Result<HeaderValue> {
    if value.is_empty() {
        return Err(credential_error());
    }
    let mut header = HeaderValue::from_str(value).map_err(|_| credential_error())?;
    header.set_sensitive(true);
    Ok(header)
}

fn headers(product: Product, secret: &SecretValue) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    match product {
        Product::CodexSubscription => {
            let auth: Value =
                serde_json::from_str(secret.expose()).map_err(|_| credential_error())?;
            let token = auth
                .pointer("/tokens/access_token")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(credential_error)?;
            let account = auth
                .pointer("/tokens/account_id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(credential_error)?;
            headers.insert("authorization", sensitive(&format!("Bearer {token}"))?);
            headers.insert("chatgpt-account-id", sensitive(account)?);
        }
        Product::GlmCoding => {
            headers.insert("authorization", sensitive(secret.expose())?);
        }
        Product::DeepseekPayg => {
            if secret.expose().is_empty() {
                return Err(credential_error());
            }
            headers.insert(
                "authorization",
                sensitive(&format!("Bearer {}", secret.expose()))?,
            );
        }
        Product::KimiCoding => return Err(unsupported()),
    }
    headers.insert("accept", HeaderValue::from_static("application/json"));
    Ok(headers)
}

#[async_trait]
impl UsageCollector for HttpUsageCollector {
    async fn collect(
        &self,
        account: &Account,
        secret: &SecretValue,
        now: Timestamp,
    ) -> Result<UsageObservation> {
        if account.product == Product::KimiCoding {
            return Err(unsupported());
        }
        if now.checked_add(TTL_MS).is_none() {
            return Err(collection_error());
        }
        let url = self
            .endpoints
            .iter()
            .find(|(product, _)| *product == account.product)
            .map(|(_, url)| url.clone())
            .ok_or_else(unsupported)?;
        let response = self
            .client
            .get(url)
            .headers(headers(account.product, secret)?)
            .send()
            .await
            .map_err(|_| collection_error())?;
        if matches!(response.status().as_u16(), 401 | 403) {
            let mut observation = empty(account.product, account.quota_owner.clone(), now);
            observation.status = CapacityStatus::ReauthenticationRequired;
            return Ok(observation);
        }
        if !response.status().is_success() {
            return Err(collection_error());
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BODY_BYTES as u64)
        {
            return Err(collection_error());
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| collection_error())?;
            if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                return Err(collection_error());
            }
            body.extend_from_slice(&chunk);
        }
        parse_usage(account.product, account.quota_owner.clone(), &body, now)
    }
}

fn empty(product: Product, owner: QuotaOwnerId, now: Timestamp) -> UsageObservation {
    UsageObservation {
        owner,
        observed_at: now,
        valid_until: now.saturating_add(TTL_MS),
        status: CapacityStatus::Unknown,
        provider_available: None,
        windows: Vec::new(),
        balances: Vec::new(),
        source: match product {
            Product::CodexSubscription => "codex.wham_usage",
            Product::GlmCoding => "glm.coding_monitor",
            Product::DeepseekPayg => "deepseek.payg_balance",
            Product::KimiCoding => "kimi.unsupported",
        }
        .into(),
    }
}

/// Parses transient provider bytes into bounded, nonsecret observations. The
/// authenticated request's selected owner supplies identity; payloads need not
/// repeat it. Fractional JSON numbers use serde_json arbitrary_precision.
pub fn parse_usage(
    product: Product,
    owner: QuotaOwnerId,
    body: &[u8],
    now: Timestamp,
) -> Result<UsageObservation> {
    if product == Product::KimiCoding {
        return Err(unsupported());
    }
    if body.len() > MAX_BODY_BYTES || now.checked_add(TTL_MS).is_none() {
        return Err(collection_error());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| collection_error())?;
    if !value.is_object() || value.get("error").is_some_and(|error| !error.is_null()) {
        return Err(collection_error());
    }
    let mut observation = empty(product, owner, now);
    match product {
        Product::CodexSubscription => codex(&value, &mut observation)?,
        Product::GlmCoding => glm(&value, &mut observation)?,
        Product::DeepseekPayg => deepseek(&value, &mut observation)?,
        Product::KimiCoding => return Err(unsupported()),
    }
    Ok(observation)
}

/// Plain decimal syntax is deliberate: unsupported exponent notation is unknown,
/// never rounded to a nearby quota threshold. Signed balances remain representable.
fn decimal(value: &Value, signed: bool) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return None,
    };
    if text.is_empty() || text.len() > 64 {
        return None;
    }
    let digits = if signed {
        text.strip_prefix('-').unwrap_or(&text)
    } else {
        &text
    };
    let (whole, fraction) = digits
        .split_once('.')
        .map_or((digits, None), |(whole, fraction)| (whole, Some(fraction)));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.is_some_and(|fraction| {
            fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit())
        })
    {
        return None;
    }
    Some(text)
}

fn percent_state(percent: Option<&str>) -> CapacityStatus {
    let Some(percent) = percent else {
        return CapacityStatus::Unknown;
    };
    let whole = percent
        .split('.')
        .next()
        .unwrap_or("")
        .trim_start_matches('0');
    if whole.len() > 3 || (whole.len() == 3 && whole >= "100") {
        CapacityStatus::Exhausted
    } else {
        CapacityStatus::Available
    }
}

fn merge(states: impl IntoIterator<Item = CapacityStatus>) -> CapacityStatus {
    let mut any = false;
    let mut unknown = false;
    for state in states {
        any = true;
        match state {
            CapacityStatus::Exhausted => return CapacityStatus::Exhausted,
            CapacityStatus::Available => {}
            _ => unknown = true,
        }
    }
    if any && !unknown {
        CapacityStatus::Available
    } else {
        CapacityStatus::Unknown
    }
}

fn label(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|v| {
        !v.is_empty()
            && v.len() <= 96
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    })
}

fn codex(value: &Value, observation: &mut UsageObservation) -> Result<()> {
    let mut states = vec![codex_bucket(value.get("rate_limit"), "codex", observation)];
    match value.get("additional_rate_limits") {
        None | Some(Value::Null) => {}
        Some(Value::Array(rows)) if rows.len() <= MAX_ROWS => {
            for (index, row) in rows.iter().enumerate() {
                let scope = label(row.get("metered_feature"));
                let key = format!("additional/{}/{index}", scope.unwrap_or("unknown"));
                // The feature name is retained in the key. Its limits do not
                // establish the base generation allowance's state.
                codex_bucket(row.get("rate_limit"), &key, observation);
            }
        }
        _ => return Err(collection_error()),
    }
    if value
        .pointer("/spend_control/reached")
        .and_then(Value::as_bool)
        == Some(true)
        || matches!(
            value
                .pointer("/rate_limit_reached_type/type")
                .and_then(Value::as_str),
            Some(
                "rate_limit_reached"
                    | "workspace_owner_credits_depleted"
                    | "workspace_member_credits_depleted"
                    | "workspace_owner_usage_limit_reached"
                    | "workspace_member_usage_limit_reached"
            )
        )
    {
        states.push(CapacityStatus::Exhausted);
    }
    observation.provider_available = value
        .pointer("/rate_limit/allowed")
        .and_then(Value::as_bool);
    observation.status = merge(states);
    if let Some(models) = value.get("model_usage").filter(|models| !models.is_null()) {
        let restrictions_unknown = models.as_object().is_none_or(|models| {
            models
                .values()
                .any(|model| model.get("available").and_then(Value::as_bool) != Some(true))
        });
        if restrictions_unknown && observation.status != CapacityStatus::Exhausted {
            observation.status = CapacityStatus::Unknown;
        }
    }
    Ok(())
}

fn codex_bucket(
    bucket: Option<&Value>,
    scope: &str,
    observation: &mut UsageObservation,
) -> CapacityStatus {
    let Some(bucket) = bucket.filter(|bucket| bucket.is_object()) else {
        return CapacityStatus::Unknown;
    };
    let mut states = Vec::new();
    for name in ["primary_window", "secondary_window"] {
        let Some(value) = bucket.get(name).filter(|v| !v.is_null()) else {
            continue;
        };
        let percent = value
            .get("used_percent")
            .and_then(|value| decimal(value, false));
        states.push(percent_state(percent.as_deref()));
        observation.windows.push(UsageWindow {
            key: format!("{scope}/{name}"),
            unit: "provider_allowance".into(),
            used: None,
            limit: None,
            used_percent: percent,
            window_seconds: value
                .get("limit_window_seconds")
                .and_then(Value::as_u64)
                .filter(|v| *v > 0),
            resets_at: value
                .get("reset_at")
                .and_then(Value::as_i64)
                .filter(|v| *v >= 0)
                .and_then(|v| v.checked_mul(1000)),
        });
    }
    if bucket.get("limit_reached").and_then(Value::as_bool) == Some(true) {
        states.push(CapacityStatus::Exhausted);
    }
    if bucket.get("allowed").and_then(Value::as_bool) != Some(true)
        || bucket
            .get("limit_reached")
            .and_then(Value::as_bool)
            .is_none()
    {
        states.push(CapacityStatus::Unknown);
    }
    merge(states)
}

fn glm(value: &Value, observation: &mut UsageObservation) -> Result<()> {
    if value
        .get("success")
        .is_some_and(|v| v.as_bool() != Some(true))
        || value
            .get("code")
            .is_some_and(|v| !matches!(v.as_i64(), Some(0 | 200)))
    {
        return Err(collection_error());
    }
    let payload = value.get("data").unwrap_or(value);
    let Some(rows) = payload.get("limits") else {
        return Ok(());
    };
    let rows = rows
        .as_array()
        .filter(|rows| rows.len() <= MAX_ROWS)
        .ok_or_else(collection_error)?;
    let mut states = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let kind = label(row.get("type")).unwrap_or("unknown");
        let unit = row.get("unit").and_then(Value::as_u64);
        let number = row.get("number").and_then(Value::as_u64);
        let percent = row
            .get("percentage")
            .and_then(|value| decimal(value, false));
        let used = row.get("currentValue").and_then(Value::as_u64);
        let limit = row.get("usage").and_then(Value::as_u64);
        let known = matches!(kind, "TOKENS_LIMIT" | "TIME_LIMIT");
        let state = if known {
            if let (Some(used), Some(limit)) = (used, limit.filter(|limit| *limit > 0)) {
                if used >= limit {
                    CapacityStatus::Exhausted
                } else if percent.is_some() {
                    percent_state(percent.as_deref())
                } else {
                    CapacityStatus::Available
                }
            } else {
                percent_state(percent.as_deref())
            }
        } else {
            CapacityStatus::Unknown
        };
        let scope_known = matches!((unit, number), (Some(3), Some(1..)) | (Some(6), Some(1..)));
        if kind != "TIME_LIMIT" {
            states.push(
                if !scope_known || (state != CapacityStatus::Exhausted && limit == Some(0)) {
                    CapacityStatus::Unknown
                } else {
                    state
                },
            );
        }
        observation.windows.push(UsageWindow {
            key: format!(
                "glm/{kind}/unit-{}/number-{}/{index}",
                unit.map_or("unknown".into(), |v| v.to_string()),
                number.map_or("unknown".into(), |v| v.to_string())
            ),
            unit: match kind {
                "TOKENS_LIMIT" => "provider_token_allowance",
                "TIME_LIMIT" => "provider_tool_allowance",
                _ => "unknown_provider_allowance",
            }
            .into(),
            used,
            limit,
            used_percent: percent,
            window_seconds: match (unit, number) {
                (Some(3), Some(number)) => number.checked_mul(3600),
                (Some(6), Some(number)) => number.checked_mul(7 * 24 * 3600),
                _ => None,
            },
            resets_at: row
                .get("nextResetTime")
                .and_then(Value::as_i64)
                .filter(|v| *v >= 0),
        });
    }
    observation.status = merge(states);
    Ok(())
}

fn deepseek(value: &Value, observation: &mut UsageObservation) -> Result<()> {
    observation.provider_available = value.get("is_available").and_then(Value::as_bool);
    // Funded is not an authorization to spend or proof of quota/concurrency headroom.
    if observation.provider_available == Some(false) {
        observation.status = CapacityStatus::Exhausted;
    }
    let Some(rows) = value.get("balance_infos") else {
        return Ok(());
    };
    let rows = rows
        .as_array()
        .filter(|rows| rows.len() <= MAX_ROWS)
        .ok_or_else(collection_error)?;
    for row in rows {
        let currency = row
            .get("currency")
            .and_then(Value::as_str)
            .filter(|v| v.len() == 3 && v.bytes().all(|b| b.is_ascii_uppercase()))
            .ok_or_else(collection_error)?;
        for (field, kind) in [
            ("total_balance", "total"),
            ("granted_balance", "granted"),
            ("topped_up_balance", "topped_up"),
        ] {
            if let Some(value) = row.get(field).filter(|value| !value.is_null()) {
                // Vendor contract specifies strings, preserving their exact spelling.
                if !value.is_string() {
                    return Err(collection_error());
                }
                let amount = decimal(value, true).ok_or_else(collection_error)?;
                observation.balances.push(Money {
                    currency: currency.into(),
                    decimal: amount,
                    kind: Some(kind.into()),
                });
            }
        }
    }
    Ok(())
}
