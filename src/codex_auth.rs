//! Codex auth.json custody helpers and one-attempt refresh exchange.
//!
//! JWT payloads are decoded only for expiry and identity consistency. Their
//! signatures are not verified here; this is not a token authenticator. The caller
//! must durably fence refresh before invoking the exchange and persist its result
//! before use. An error after issuance never authorizes retrying the old token.
use std::{fmt, net::IpAddr, time::Duration};

use base64::{
    Engine,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    domain::{Error, ErrorCode, Result, Timestamp},
    ports::SecretValue,
};

const MAX_BUNDLE: usize = 256 * 1024;
const MAX_TOKEN: usize = 64 * 1024;

fn invalid() -> Error {
    Error::new(
        ErrorCode::CredentialUnavailable,
        "Codex credential bundle or token fields are invalid",
    )
}
fn identity_mismatch() -> Error {
    Error::new(
        ErrorCode::ReauthenticationRequired,
        "Codex credential identity changed or is inconsistent",
    )
}
fn refresh_failed() -> Error {
    Error::new(
        ErrorCode::ReauthenticationRequired,
        "Codex refresh did not produce a usable bundle; reconcile credential state before retrying",
    )
}

pub struct CodexAuth {
    original: Value,
    account_id: String,
    subject: Option<String>,
}
impl fmt::Debug for CodexAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CodexAuth([REDACTED])")
    }
}
impl CodexAuth {
    pub fn parse(secret: &SecretValue) -> Result<Self> {
        if secret.expose().len() > MAX_BUNDLE {
            return Err(invalid());
        }
        let original: Value = serde_json::from_str(secret.expose()).map_err(|_| invalid())?;
        let root = original.as_object().ok_or_else(invalid)?;
        let tokens = root
            .get("tokens")
            .and_then(Value::as_object)
            .ok_or_else(invalid)?;
        let account_id = token_field(tokens, "account_id", true)?
            .ok_or_else(invalid)?
            .to_owned();
        let access = token_field(tokens, "access_token", true)?.ok_or_else(invalid)?;
        token_field(tokens, "refresh_token", false)?;
        let id = token_field(tokens, "id_token", false)?;
        if let Some(id) = id {
            jwt_claims(id)?.ok_or_else(invalid)?;
        }
        let mut subject = None;
        for token in [Some(access), id].into_iter().flatten() {
            if let Some(claims) = jwt_claims(token)? {
                check_claims(&claims, &account_id, &mut subject)?;
            }
        }
        Ok(Self {
            original,
            account_id,
            subject,
        })
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Unverified subject claim, kept separate from the account/workspace identity.
    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    /// Missing, opaque or invalid expiry is unknown and therefore needs refresh.
    /// Decoding an exp field does not validate the token's signature or entitlement.
    pub fn needs_refresh(&self, now_ms: Timestamp, skew_ms: i64) -> Result<bool> {
        if now_ms < 0 || skew_ms < 0 {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Refresh time and skew must be nonnegative",
            ));
        }
        let Some(threshold) = now_ms.checked_add(skew_ms) else {
            return Ok(true);
        };
        let access = self.original["tokens"]["access_token"]
            .as_str()
            .ok_or_else(invalid)?;
        let Some(claims) = jwt_claims(access)? else {
            return Ok(true);
        };
        let Some(expiry) = claims
            .get("exp")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
            .and_then(|value| value.checked_mul(1000))
        else {
            return Ok(true);
        };
        Ok(expiry <= threshold)
    }
}

fn token_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    required: bool,
) -> Result<Option<&'a str>> {
    match object.get(name) {
        None if !required => Ok(None),
        Some(Value::String(value))
            if !value.is_empty()
                && value.len() <= MAX_TOKEN
                && value.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            Ok(Some(value))
        }
        _ => Err(invalid()),
    }
}

/// Opaque bearer strings have no inspectable claims. JWT-shaped strings must decode.
fn jwt_claims(token: &str) -> Result<Option<Map<String, Value>>> {
    if !token.contains('.') {
        return Ok(None);
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(invalid());
    }
    let payload = URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| URL_SAFE.decode(parts[1]))
        .map_err(|_| invalid())?;
    let value: Value = serde_json::from_slice(&payload).map_err(|_| invalid())?;
    value.as_object().cloned().map(Some).ok_or_else(invalid)
}

fn check_claims(
    claims: &Map<String, Value>,
    account: &str,
    subject: &mut Option<String>,
) -> Result<()> {
    let nested = match claims.get("https://api.openai.com/auth") {
        None => None,
        Some(Value::Object(value)) => Some(value),
        _ => return Err(invalid()),
    };
    for account_claim in [
        claims.get("chatgpt_account_id"),
        nested.and_then(|value| value.get("chatgpt_account_id")),
    ]
    .into_iter()
    .flatten()
    {
        match account_claim.as_str() {
            Some(value) if value == account => (),
            Some(_) => return Err(identity_mismatch()),
            None => return Err(invalid()),
        }
    }
    if let Some(value) = claims.get("sub") {
        let value = value
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(invalid)?;
        if subject.as_ref().is_some_and(|previous| previous != value) {
            return Err(identity_mismatch());
        }
        *subject = Some(value.to_owned());
    }
    Ok(())
}

pub struct CodexRefresher {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    client_id: String,
}
impl fmt::Debug for CodexRefresher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CodexRefresher([REDACTED])")
    }
}
impl CodexRefresher {
    /// Endpoint and OAuth client registration are trusted explicit configuration.
    pub fn new(endpoint: String, client_id: String, allow_loopback_http: bool) -> Result<Self> {
        let invalid_config = || {
            Error::new(
                ErrorCode::InvalidInput,
                "Invalid Codex refresh endpoint or client registration",
            )
        };
        let endpoint = reqwest::Url::parse(&endpoint).map_err(|_| invalid_config())?;
        let loopback = endpoint.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        if endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !(endpoint.scheme() == "https"
                || (allow_loopback_http && endpoint.scheme() == "http" && loopback))
            || client_id.is_empty()
            || client_id.len() > 256
            || !client_id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(invalid_config());
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| invalid_config())?;
        Ok(Self {
            client,
            endpoint,
            client_id,
        })
    }

    /// Performs exactly one refresh exchange. No implicit inference or refresh retry.
    pub async fn refresh(&self, current: &SecretValue, now: Timestamp) -> Result<SecretValue> {
        let mut auth = CodexAuth::parse(current)?;
        if now < 0 {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Refresh time must be nonnegative",
            ));
        }
        let refreshed_at = OffsetDateTime::from_unix_timestamp_nanos(i128::from(now) * 1_000_000)
            .map_err(|_| {
                Error::new(
                    ErrorCode::InvalidInput,
                    "Refresh time is outside RFC3339 range",
                )
            })?
            .format(&Rfc3339)
            .map_err(|_| {
                Error::new(
                    ErrorCode::InvalidInput,
                    "Refresh time is outside RFC3339 range",
                )
            })?;
        let refresh = auth.original["tokens"]
            .as_object()
            .and_then(|tokens| tokens.get("refresh_token"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::ReauthenticationRequired,
                    "Codex refresh token is unavailable",
                )
            })?;
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&json!({
                "grant_type":"refresh_token", "refresh_token":refresh, "client_id":self.client_id,
            }))
            .send()
            .await
            .map_err(|_| refresh_failed())?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_BUNDLE as u64)
        {
            return Err(refresh_failed());
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| refresh_failed())?;
            if chunk.len() > MAX_BUNDLE.saturating_sub(body.len()) {
                return Err(refresh_failed());
            }
            body.extend_from_slice(&chunk);
        }
        let response: Value = serde_json::from_slice(&body).map_err(|_| refresh_failed())?;
        let fields = response.as_object().ok_or_else(refresh_failed)?;
        token_field(fields, "access_token", true).map_err(|_| refresh_failed())?;
        token_field(fields, "refresh_token", false).map_err(|_| refresh_failed())?;
        token_field(fields, "id_token", false).map_err(|_| refresh_failed())?;
        if let Some(Value::String(id)) = fields.get("id_token") {
            jwt_claims(id)
                .map_err(|_| refresh_failed())?
                .ok_or_else(refresh_failed)?;
        }
        for field in ["access_token", "id_token"] {
            if let Some(Value::String(token)) = fields.get(field)
                && let Some(claims) = jwt_claims(token).map_err(|_| refresh_failed())?
            {
                check_claims(&claims, &auth.account_id, &mut auth.subject)
                    .map_err(|_| refresh_failed())?;
            }
        }
        let tokens = auth.original["tokens"]
            .as_object_mut()
            .ok_or_else(invalid)?;
        for field in ["access_token", "refresh_token", "id_token"] {
            if let Some(value) = fields.get(field) {
                tokens.insert(field.into(), value.clone());
            }
        }
        auth.original["last_refresh"] = Value::String(refreshed_at);
        let merged =
            SecretValue::new(serde_json::to_string(&auth.original).map_err(|_| refresh_failed())?);
        CodexAuth::parse(&merged).map_err(|_| refresh_failed())?;
        Ok(merged)
    }
}
