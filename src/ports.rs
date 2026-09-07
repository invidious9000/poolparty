//! Interfaces shared by persistence, the application and provider adapters.
use crate::domain::*;
use async_trait::async_trait;
use bytes::Bytes;
use futures_core::Stream;
use std::{fmt, pin::Pin};

#[async_trait]
pub trait Ledger: Send + Sync {
    /// Return only authorized accounts and their visible pool memberships.
    async fn accounts(&self, principal: &Principal) -> Result<Vec<Account>>;
    async fn set_account_enabled(&self, id: &AccountId, enabled: bool) -> Result<()>;
    async fn usage_observation(&self, owner: &QuotaOwnerId) -> Result<Option<UsageObservation>>;
    /// Persist a monotonic credential generation before external credential operations.
    async fn advance_credential_generation(&self, reference: &CredentialRef) -> Result<()>;
    async fn put_account(&self, account: Account) -> Result<()>;
    async fn put_quota_policy(&self, policy: QuotaPolicy) -> Result<()>;
    /// Older observations never replace newer evidence, including auth/quota failures.
    async fn observe(&self, observation: UsageObservation) -> Result<()>;
    async fn create_binding(
        &self,
        principal: &Principal,
        intent: CreateBinding,
        now: Timestamp,
    ) -> Result<Binding>;
    async fn binding(&self, principal: &Principal, id: &BindingId) -> Result<Binding>;
    async fn close_binding(
        &self,
        principal: &Principal,
        id: &BindingId,
        now: Timestamp,
    ) -> Result<Binding>;
    async fn admit(
        &self,
        principal: &Principal,
        admission: Admission,
        now: Timestamp,
    ) -> Result<PreparedAttempt>;
    /// Durable dispatch intent is committed before invoking the transport.
    async fn mark_dispatching(&self, id: &AttemptId, now: Timestamp) -> Result<()>;
    async fn mark_streaming(&self, id: &AttemptId, now: Timestamp) -> Result<()>;
    async fn settle(&self, id: &AttemptId, outcome: Settlement, now: Timestamp) -> Result<Attempt>;
    async fn attempt(&self, principal: &Principal, id: &AttemptId) -> Result<Attempt>;
    /// Startup under exclusive process ownership: release reserved, fence possibly sent work.
    async fn recover(&self, now: Timestamp) -> Result<()>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

#[async_trait]
pub trait RequestPreparation: Send + Sync {
    /// Refresh account state before admission. Failure is always pre-dispatch.
    async fn prepare(&self, binding: &Binding) -> Result<()>;
}

/// Deliberately lacks Serialize and exposes only a redacted Debug implementation.
pub struct SecretValue(String);
impl SecretValue {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue([REDACTED])")
    }
}

#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn load(&self, reference: &CredentialRef) -> Result<SecretValue>;
    /// Compare the complete expected generation; never overwrite a newer bundle.
    async fn replace(&self, expected: &CredentialRef, next: SecretValue) -> Result<CredentialRef>;
}

#[async_trait]
pub trait VersionedCredentialStore: CredentialStore {
    async fn latest(&self, id: &CredentialId) -> Result<(CredentialRef, SecretValue)>;
}

#[async_trait]
pub trait CredentialRefresher: Send + Sync {
    /// One exchange. Ambiguous issuance must never be retried automatically.
    async fn refresh(&self, current: &SecretValue, now: Timestamp) -> Result<SecretValue>;
}

#[async_trait]
pub trait UsageCollector: Send + Sync {
    async fn collect(
        &self,
        account: &Account,
        secret: &SecretValue,
        now: Timestamp,
    ) -> Result<UsageObservation>;
}

pub struct UpstreamRequest {
    pub prepared: PreparedAttempt,
    pub protocol: Protocol,
    pub body: Bytes,
    pub secret: SecretValue,
}

#[derive(Clone, Debug)]
pub struct TransportError {
    pub certainty: DispatchCertainty,
    pub code: ErrorCode,
}

pub enum StreamEvent {
    Data(Bytes),
    /// The final raw chunk and its evidence travel together, so the application
    /// persists completion before delivering a terminal event to the caller.
    Terminal {
        bytes: Bytes,
        outcome: Settlement,
    },
    /// Proven native terminal event or completed rejection, never mere socket EOF.
    Finished(Settlement),
}

pub type ProviderStream =
    Pin<Box<dyn Stream<Item = std::result::Result<StreamEvent, TransportError>> + Send>>;
pub struct UpstreamResponse {
    pub status: u16,
    /// Allowlisted response metadata only. Never upstream authentication headers.
    pub headers: Vec<(String, String)>,
    pub stream: ProviderStream,
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// Exactly one upstream attempt. No redirects, fallback or automatic retry.
    async fn send(
        &self,
        request: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse, TransportError>;
}
