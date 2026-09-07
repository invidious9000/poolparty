//! Application orchestration. The ledger owns policy and transactions; no lock spans a stream.
use std::{
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::{domain::*, ports::*};

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64
    }
}

#[derive(Clone)]
pub struct Router {
    ledger: Arc<dyn Ledger>,
    transport: Arc<dyn Transport>,
    credentials: Arc<dyn CredentialStore>,
    clock: Arc<dyn Clock>,
}

pub struct RoutedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub attempt_id: AttemptId,
    pub stream: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>,
}

impl Router {
    pub fn new(
        ledger: Arc<dyn Ledger>,
        transport: Arc<dyn Transport>,
        credentials: Arc<dyn CredentialStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            ledger,
            transport,
            credentials,
            clock,
        }
    }

    pub fn ledger(&self) -> &Arc<dyn Ledger> {
        &self.ledger
    }
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    pub async fn execute(
        &self,
        principal: &Principal,
        binding: BindingId,
        operation: Option<OperationId>,
        protocol: Protocol,
        body: Bytes,
    ) -> Result<RoutedResponse> {
        // A dropped HTTP future must not abandon a spawn_blocking admission after
        // it commits but before we receive its attempt ID. The detached task finishes
        // constructing the guard; an undelivered response is then dropped normally.
        let this = self.clone();
        let principal = principal.clone();
        let error_binding = binding.clone();
        tokio::spawn(async move {
            this.execute_inner(principal, binding, operation, protocol, body)
                .await
        })
        .await
        .map_err(|_| {
            uncertain_error(
                Error::new(
                    ErrorCode::StorageUnavailable,
                    "Request coordination failed; inspect state before retrying.",
                ),
                &error_binding,
            )
        })?
    }

    async fn execute_inner(
        &self,
        principal: Principal,
        binding_id: BindingId,
        operation: Option<OperationId>,
        protocol: Protocol,
        body: Bytes,
    ) -> Result<RoutedResponse> {
        let binding = self.ledger.binding(&principal, &binding_id).await?;
        if binding.intent.product.protocol() != protocol {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "Protocol does not match the bound product.",
            )
            .bound(&binding_id));
        }
        let (model, effort) = validate_body(protocol, &body).map_err(|e| e.bound(&binding_id))?;
        if protocol == Protocol::Messages && binding.intent.effort.is_some() {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "Messages effort mapping is not implemented.",
            )
            .bound(&binding_id));
        }
        let prepared = self
            .ledger
            .admit(
                &principal,
                Admission {
                    binding: binding_id.clone(),
                    operation,
                    request_fingerprint: format!("{:x}", Sha256::digest(&body)),
                    model,
                    effort,
                },
                self.now(),
            )
            .await?;
        let id = prepared.attempt.id.clone();
        let mut guard = AttemptGuard::new(self.ledger.clone(), self.clock.clone(), id.clone());
        let secret = match self.credentials.load(&prepared.attempt.credential).await {
            Ok(value) => value,
            Err(_) => {
                guard.finish(Settlement::NotDispatched).await?;
                return Err(Error::new(
                    ErrorCode::CredentialUnavailable,
                    "The selected credential generation is unavailable.",
                )
                .bound(&binding_id));
            }
        };
        // Arm conservatively before awaiting the durable transition.
        guard.on_drop = Settlement::Uncertain;
        self.ledger
            .mark_dispatching(&id, self.now())
            .await
            .map_err(|e| uncertain_error(e, &binding_id).with_attempt(&id))?;
        let upstream = match self
            .transport
            .send(UpstreamRequest {
                prepared,
                protocol,
                body,
                secret,
            })
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let settlement = if error.certainty == DispatchCertainty::NotDispatched {
                    Settlement::NotDispatched
                } else {
                    Settlement::Uncertain
                };
                guard
                    .finish(settlement)
                    .await
                    .map_err(|e| uncertain_error(e, &binding_id).with_attempt(&id))?;
                let mut result = Error::new(
                    error.code,
                    "The upstream attempt failed; no automatic retry was made.",
                )
                .bound(&binding_id)
                .with_attempt(&id);
                result.request_state = error.certainty;
                return Err(result);
            }
        };
        self.ledger
            .mark_streaming(&id, self.now())
            .await
            .map_err(|e| uncertain_error(e, &binding_id).with_attempt(&id))?;
        let UpstreamResponse {
            status,
            headers,
            mut stream,
        } = upstream;
        let error_attempt = id.clone();
        let output = async_stream::stream! {
            while let Some(event) = stream.next().await {
                match event {
                    Ok(StreamEvent::Data(bytes)) => yield Ok(bytes),
                    Ok(StreamEvent::Terminal { bytes, outcome }) => {
                        if !matches!(outcome, Settlement::Succeeded | Settlement::Rejected) {
                            let error = uncertain_error(Error::new(ErrorCode::UpstreamUnavailable, "Invalid upstream completion signal."), &binding_id).with_attempt(&error_attempt);
                            yield Err(error);
                            return;
                        }
                        if let Err(error) = guard.finish(outcome).await {
                            yield Err(uncertain_error(error, &binding_id).with_attempt(&error_attempt));
                            return;
                        }
                        yield Ok(bytes);
                        return;
                    }
                    Ok(StreamEvent::Finished(outcome)) => {
                        if !matches!(outcome, Settlement::Succeeded | Settlement::Rejected) {
                            let mut e = Error::new(ErrorCode::UpstreamUnavailable, "Invalid upstream completion signal.").bound(&binding_id).with_attempt(&error_attempt);
                            e.request_state = DispatchCertainty::Unknown;
                            yield Err(e);
                            return;
                        }
                        if let Err(error) = guard.finish(outcome).await {
                            yield Err(uncertain_error(error, &binding_id).with_attempt(&error_attempt));
                            return;
                        }
                        return;
                    }
                    Err(_) => {
                        let _ = guard.finish(Settlement::Uncertain).await;
                        let mut error = Error::new(ErrorCode::UpstreamUnavailable, "Upstream stream interrupted; execution remains uncertain.").bound(&binding_id).with_attempt(&error_attempt);
                        error.request_state = DispatchCertainty::Unknown;
                        yield Err(error);
                        return;
                    }
                }
            }
            let _ = guard.finish(Settlement::Uncertain).await;
            let mut error = Error::new(ErrorCode::UpstreamUnavailable, "Upstream ended without a confirmed terminal event.").bound(&binding_id).with_attempt(&error_attempt);
            error.request_state = DispatchCertainty::Unknown;
            yield Err(error);
        };
        Ok(RoutedResponse {
            status,
            headers,
            attempt_id: id,
            stream: Box::pin(output),
        })
    }
}

fn uncertain_error(error: Error, binding: &BindingId) -> Error {
    let mut error = error.bound(binding);
    error.request_state = DispatchCertainty::Unknown;
    error
}

fn validate_body(protocol: Protocol, body: &[u8]) -> Result<(String, Option<String>)> {
    if body.len() > 2 * 1024 * 1024 {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Request exceeds the body limit.",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| Error::new(ErrorCode::InvalidInput, "Expected a JSON object."))?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInput, "Expected a JSON object."))?;
    let model = object
        .get("model")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty() && m.len() <= 256)
        .ok_or_else(|| Error::new(ErrorCode::InvalidInput, "A bounded model name is required."))?;
    if object.get("stream").and_then(|s| s.as_bool()) != Some(true) {
        return Err(Error::new(
            ErrorCode::Unsupported,
            "Only streaming inference is implemented.",
        ));
    }
    if protocol == Protocol::Responses {
        if ["previous_response_id", "conversation"]
            .iter()
            .any(|key| object.get(*key).is_some_and(|v| !v.is_null()))
            || object.get("store").and_then(|s| s.as_bool()) == Some(true)
            || has_remote_reference(&value)
        {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "Remote continuation and file ownership are not implemented; supply self-contained input.",
            ));
        }
        let effort = match object.get("reasoning").and_then(|v| v.get("effort")) {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(value)) if !value.is_empty() && value.len() <= 32 => {
                Some(value.clone())
            }
            _ => {
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    "Invalid reasoning effort.",
                ));
            }
        };
        Ok((model.into(), effort))
    } else {
        Ok((model.into(), None))
    }
}

fn has_remote_reference(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key("file_id")
                || map.get("type").and_then(|v| v.as_str()) == Some("item_reference")
                || map.values().any(has_remote_reference)
        }
        serde_json::Value::Array(values) => values.iter().any(has_remote_reference),
        _ => false,
    }
}

struct AttemptGuard {
    ledger: Arc<dyn Ledger>,
    clock: Arc<dyn Clock>,
    id: AttemptId,
    on_drop: Settlement,
    armed: bool,
}
impl AttemptGuard {
    fn new(ledger: Arc<dyn Ledger>, clock: Arc<dyn Clock>, id: AttemptId) -> Self {
        Self {
            ledger,
            clock,
            id,
            on_drop: Settlement::NotDispatched,
            armed: true,
        }
    }
    async fn finish(&mut self, outcome: Settlement) -> Result<()> {
        self.ledger
            .settle(&self.id, outcome, self.clock.now())
            .await?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if self.armed
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let ledger = self.ledger.clone();
            let id = self.id.clone();
            let now = self.clock.now();
            let outcome = self.on_drop;
            handle.spawn(async move {
                let _ = ledger.settle(&id, outcome, now).await;
            });
        }
        // If the executor is gone, startup recovery fences the durable dispatch intent.
    }
}
