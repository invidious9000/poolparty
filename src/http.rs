//! Authenticated control and native streaming routes. Static grants are a local scaffold.
use std::{collections::BTreeSet, fmt, sync::Arc};

use axum::{
    Extension, Json, Router as HttpRouter,
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{domain::*, ports::SecretValue, runtime::Router};

const BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Immutable process-local grants. Reconfigure/restart to replace or revoke grants.
pub struct BearerGrants(Vec<([u8; 32], Principal)>);

impl fmt::Debug for BearerGrants {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BearerGrants")
            .field("count", &self.0.len())
            .finish()
    }
}

impl BearerGrants {
    pub fn new(grants: Vec<(SecretValue, Principal)>) -> Result<Self> {
        let mut entries: Vec<([u8; 32], Principal)> = Vec::new();
        for (secret, principal) in grants {
            if secret.expose().is_empty() || !secret.expose().bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    "Grant tokens must be nonempty visible ASCII",
                ));
            }
            let digest: [u8; 32] = Sha256::digest(secret.expose().as_bytes()).into();
            if entries.iter().any(|(existing, _)| existing == &digest) {
                return Err(Error::new(ErrorCode::InvalidInput, "Duplicate grant token"));
            }
            entries.push((digest, principal));
        }
        Ok(Self(entries))
    }

    fn authenticate(&self, headers: &HeaderMap) -> Result<Principal> {
        let unauthorized =
            || Error::new(ErrorCode::Unauthorized, "A valid Bearer grant is required");
        if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
            return Err(unauthorized());
        }
        let value = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(unauthorized());
        }
        let candidate: [u8; 32] = Sha256::digest(value.as_bytes()).into();
        let mut found = None;
        for (expected, principal) in &self.0 {
            let difference = expected
                .iter()
                .zip(candidate.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b));
            if difference == 0 {
                found = Some(principal.clone());
            }
        }
        found.ok_or_else(unauthorized)
    }
}

struct AppState {
    runtime: Arc<Router>,
    grants: BearerGrants,
}

pub fn app(runtime: Arc<Router>, grants: BearerGrants) -> HttpRouter {
    let state = Arc::new(AppState { runtime, grants });
    let protected = HttpRouter::new()
        .route("/api/v1/accounts", get(inspect_accounts))
        .route("/api/v1/sessions", post(create_binding))
        .route("/api/v1/sessions/{id}", get(inspect_binding))
        .route("/api/v1/sessions/{id}/close", post(close_binding))
        .route("/api/v1/attempts/{id}", get(inspect_attempt))
        .route("/routes/{id}/v1/messages", any(messages))
        .route("/routes/{id}/codex/responses", any(responses))
        .route(
            "/routes/{id}/codex/responses/compact",
            any(unsupported_responses),
        )
        .route(
            "/routes/{id}/v1/messages/count_tokens",
            any(unsupported_messages),
        )
        .fallback(unsupported)
        .method_not_allowed_fallback(unsupported)
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state);
    HttpRouter::new()
        .route("/healthz", get(|| async { Json(json!({"status": "ok"})) }))
        .merge(protected)
}

fn protocol(path: &str) -> Protocol {
    if path.contains("/v1/messages") {
        Protocol::Messages
    } else {
        Protocol::Responses
    }
}

async fn authenticate(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    match state.grants.authenticate(request.headers()) {
        Ok(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Err(error) => error_response(error, protocol(request.uri().path())),
    }
}

fn binding_id(value: String) -> Result<BindingId> {
    BindingId::new(value).map_err(|_| Error::new(ErrorCode::InvalidInput, "Invalid binding ID"))
}

/// Explicit projection keeps credential references out of the caller control plane.
#[derive(Serialize)]
struct AccountStatus {
    id: AccountId,
    product: Product,
    pools: BTreeSet<PoolId>,
    models: BTreeSet<String>,
    enabled: bool,
    quota_owner: QuotaOwnerId,
    credential_generation: u64,
    usage: Option<UsageObservation>,
}

async fn inspect_accounts(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
) -> Response {
    let result = async {
        let mut accounts = Vec::new();
        for account in state.runtime.ledger().accounts(&principal).await? {
            let pools: BTreeSet<_> = account
                .pools
                .intersection(&principal.pools)
                .cloned()
                .collect();
            if pools.is_empty() {
                continue;
            }
            let usage = state
                .runtime
                .ledger()
                .usage_observation(&account.quota_owner)
                .await?;
            accounts.push(AccountStatus {
                id: account.id,
                product: account.product,
                pools,
                models: account.models,
                enabled: account.enabled,
                quota_owner: account.quota_owner,
                credential_generation: account.credential.generation,
                usage,
            });
        }
        Result::<_>::Ok(accounts)
    }
    .await;
    match result {
        Ok(accounts) => {
            let mut response = Json(json!({"accounts":accounts})).into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(error) => error_response(error, Protocol::Responses),
    }
}

async fn create_binding(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> Response {
    let result = async {
        let bytes = to_bytes(request.into_body(), BODY_LIMIT)
            .await
            .map_err(|_| {
                Error::new(
                    ErrorCode::InvalidInput,
                    "Request body could not be read within the 2 MiB limit",
                )
            })?;
        let intent: CreateBinding = serde_json::from_slice(&bytes)
            .map_err(|_| Error::new(ErrorCode::InvalidInput, "Invalid session request"))?;
        state
            .runtime
            .ledger()
            .create_binding(&principal, intent, state.runtime.now())
            .await
    }
    .await;
    match result {
        Ok(binding) => (StatusCode::CREATED, Json(binding)).into_response(),
        Err(e) => error_response(e, Protocol::Responses),
    }
}

async fn inspect_binding(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        state
            .runtime
            .ledger()
            .binding(&principal, &binding_id(id)?)
            .await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error_response(e, Protocol::Responses),
    }
}

async fn close_binding(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        state
            .runtime
            .ledger()
            .close_binding(&principal, &binding_id(id)?, state.runtime.now())
            .await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error_response(e, Protocol::Responses),
    }
}

async fn inspect_attempt(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let id = AttemptId::new(id)
            .map_err(|_| Error::new(ErrorCode::InvalidInput, "Invalid attempt ID"))?;
        state.runtime.ledger().attempt(&principal, &id).await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error_response(e, Protocol::Responses),
    }
}

async fn messages(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    route_model(state, principal, id, Protocol::Messages, request).await
}
async fn responses(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    route_model(state, principal, id, Protocol::Responses, request).await
}

async fn route_model(
    state: Arc<AppState>,
    principal: Principal,
    id: String,
    protocol: Protocol,
    request: Request,
) -> Response {
    let error_binding = BindingId::new(id.clone()).ok();
    let result = async {
        let id = binding_id(id)?;
        // Authorize before parsing the request or disclosing bound endpoint capabilities.
        state.runtime.ledger().binding(&principal, &id).await?;
        if request.method() != Method::POST || request.headers().contains_key(header::UPGRADE) {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "Only HTTP POST streaming is supported",
            )
            .bound(&id));
        }
        let operation = parse_operation(request.headers()).map_err(|e| e.bound(&id))?;
        let bytes = to_bytes(request.into_body(), BODY_LIMIT)
            .await
            .map_err(|_| {
                Error::new(
                    ErrorCode::InvalidInput,
                    "Request body could not be read within the 2 MiB limit",
                )
                .bound(&id)
            })?;
        state
            .runtime
            .execute(&principal, id, operation, protocol, bytes)
            .await
    }
    .await;
    match result {
        Err(error) => error_response(error, protocol),
        Ok(routed) => {
            let status = match StatusCode::from_u16(routed.status) {
                Ok(status)
                    if status.is_success()
                        || status.is_client_error()
                        || status.is_server_error() =>
                {
                    status
                }
                _ => {
                    let mut error = Error::new(
                        ErrorCode::UpstreamUnavailable,
                        "Invalid upstream status; inspect the binding before retrying",
                    );
                    if let Some(binding) = error_binding {
                        error = error.bound(&binding);
                    }
                    error.request_state = DispatchCertainty::Unknown;
                    let mut response = error_response(error, protocol);
                    if let Ok(value) = HeaderValue::from_str(routed.attempt_id.as_str()) {
                        response
                            .headers_mut()
                            .insert("x-poolparty-attempt-id", value);
                    }
                    return response;
                }
            };
            let mut response = Response::new(Body::from_stream(routed.stream));
            *response.status_mut() = status;
            for (name, value) in routed.headers {
                if !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "content-type"
                        | "cache-control"
                        | "retry-after"
                        | "request-id"
                        | "x-request-id"
                ) {
                    continue;
                }
                if let (Ok(name), Ok(value)) =
                    (HeaderName::try_from(name), HeaderValue::try_from(value))
                {
                    response.headers_mut().insert(name, value);
                }
            }
            if let Ok(value) = HeaderValue::from_str(routed.attempt_id.as_str()) {
                response
                    .headers_mut()
                    .insert("x-poolparty-attempt-id", value);
            }
            response
        }
    }
}

fn parse_operation(headers: &HeaderMap) -> Result<Option<OperationId>> {
    let mut values = headers.get_all("x-poolparty-operation-id").iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Duplicate operation ID header",
        ));
    }
    let value = value
        .to_str()
        .map_err(|_| Error::new(ErrorCode::InvalidInput, "Invalid operation ID"))?;
    OperationId::new(value)
        .map(Some)
        .map_err(|_| Error::new(ErrorCode::InvalidInput, "Invalid operation ID"))
}

async fn unsupported_responses(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    unsupported_bound(state, principal, id, Protocol::Responses).await
}
async fn unsupported_messages(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    unsupported_bound(state, principal, id, Protocol::Messages).await
}
async fn unsupported_bound(
    state: Arc<AppState>,
    principal: Principal,
    id: String,
    protocol: Protocol,
) -> Response {
    let result = async {
        let id = binding_id(id)?;
        state.runtime.ledger().binding(&principal, &id).await?;
        Result::<()>::Err(
            Error::new(ErrorCode::Unsupported, "Endpoint is not supported").bound(&id),
        )
    }
    .await;
    error_response(result.unwrap_err(), protocol)
}
async fn unsupported(request: Request) -> Response {
    error_response(
        Error::new(ErrorCode::Unsupported, "Endpoint is not supported"),
        protocol(request.uri().path()),
    )
}

fn error_response(error: Error, protocol: Protocol) -> Response {
    let status = match error.code {
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        ErrorCode::SessionQuotaExhausted | ErrorCode::SessionConcurrencyExhausted => {
            StatusCode::TOO_MANY_REQUESTS
        }
        ErrorCode::Closed
        | ErrorCode::IntentConflict
        | ErrorCode::SessionUncertain
        | ErrorCode::OperationConflict
        | ErrorCode::OperationAlreadyExists
        | ErrorCode::InvalidTransition => StatusCode::CONFLICT,
        ErrorCode::Unsupported => StatusCode::NOT_IMPLEMENTED,
        ErrorCode::NoEligibleAccount
        | ErrorCode::CapacityUnknown
        | ErrorCode::ReauthenticationRequired
        | ErrorCode::StorageUnavailable
        | ErrorCode::CredentialUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::UpstreamUnavailable => StatusCode::BAD_GATEWAY,
    };
    let kind = match status {
        StatusCode::UNAUTHORIZED => "authentication_error",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        s if s.is_server_error() => "api_error",
        _ => "invalid_request_error",
    };
    let mut detail = serde_json::to_value(error).expect("error fields are JSON serializable");
    detail["type"] = json!(kind);
    let body = match protocol {
        Protocol::Responses => json!({"error": detail}),
        Protocol::Messages => json!({"type": "error", "error": detail}),
    };
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}
