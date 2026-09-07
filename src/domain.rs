use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> std::result::Result<Self, String> {
                Self::try_from(value.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
                if value.is_empty()
                    || value.len() > 128
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                {
                    return Err(concat!(
                        stringify!($name),
                        " must contain 1..128 ASCII letters, digits, dash, underscore or dot"
                    )
                    .into());
                }
                Ok(Self(value))
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}
id!(AccountId);
id!(QuotaOwnerId);
id!(CredentialId);
id!(PoolId);
id!(PrincipalId);
id!(ClientSessionId);
id!(BindingId);
id!(AttemptId);
id!(OperationId);

/// Unix milliseconds, supplied by the application clock rather than hidden in storage.
pub type Timestamp = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Product {
    CodexSubscription,
    KimiCoding,
    GlmCoding,
    DeepseekPayg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Responses,
    Messages,
}

impl Product {
    pub fn protocol(self) -> Protocol {
        match self {
            Self::CodexSubscription => Protocol::Responses,
            _ => Protocol::Messages,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRef {
    pub id: CredentialId,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub product: Product,
    pub quota_owner: QuotaOwnerId,
    pub pools: BTreeSet<PoolId>,
    pub credential: CredentialRef,
    pub models: BTreeSet<String>,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownCapacityPolicy {
    Reject,
    AllowUnderLocalCap,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaPolicy {
    pub owner: QuotaOwnerId,
    /// A local safety cap, never represented as a discovered provider ceiling.
    pub max_concurrency: u32,
    pub unknown: UnknownCapacityPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityStatus {
    Available,
    Exhausted,
    Unknown,
    ReauthenticationRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub key: String,
    pub unit: String,
    pub used: Option<u64>,
    pub limit: Option<u64>,
    pub resets_at: Option<Timestamp>,
    #[serde(default)]
    pub used_percent: Option<String>,
    #[serde(default)]
    pub window_seconds: Option<u64>,
}

/// Decimal text preserves provider currency amounts without floating-point rounding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    pub currency: String,
    pub decimal: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageObservation {
    pub owner: QuotaOwnerId,
    pub observed_at: Timestamp,
    pub valid_until: Timestamp,
    pub status: CapacityStatus,
    pub windows: Vec<UsageWindow>,
    pub balances: Vec<Money>,
    pub source: String,
    #[serde(default)]
    pub provider_available: Option<bool>,
}

/// Constructed by an authenticator. Never deserialized from a request body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub id: PrincipalId,
    pub pools: BTreeSet<PoolId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBinding {
    pub session: ClientSessionId,
    pub pool: PoolId,
    pub product: Product,
    pub model: String,
    pub account: Option<AccountId>,
    pub effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub id: BindingId,
    pub principal: PrincipalId,
    pub intent: CreateBinding,
    pub account: AccountId,
    pub created_at: Timestamp,
    pub closed_at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Reserved,
    Dispatching,
    Streaming,
    Uncertain,
    Succeeded,
    Rejected,
    NotDispatched,
}

impl AttemptState {
    pub fn holds_capacity(self) -> bool {
        matches!(
            self,
            Self::Reserved | Self::Dispatching | Self::Streaming | Self::Uncertain
        )
    }
    pub fn certainty(self) -> DispatchCertainty {
        match self {
            Self::Reserved | Self::NotDispatched => DispatchCertainty::NotDispatched,
            Self::Dispatching | Self::Uncertain => DispatchCertainty::Unknown,
            _ => DispatchCertainty::Dispatched,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchCertainty {
    NotDispatched,
    Dispatched,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: AttemptId,
    pub binding: BindingId,
    pub quota_owner: QuotaOwnerId,
    pub operation: Option<OperationId>,
    /// Hash only: request bodies and transcripts do not belong in the ledger.
    pub request_fingerprint: String,
    pub credential: CredentialRef,
    pub state: AttemptState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct Admission {
    pub binding: BindingId,
    pub operation: Option<OperationId>,
    pub request_fingerprint: String,
    pub model: String,
    pub effort: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PreparedAttempt {
    pub attempt: Attempt,
    pub binding: Binding,
    pub account: Account,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settlement {
    Succeeded,
    Rejected,
    NotDispatched,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    NotFound,
    Closed,
    IntentConflict,
    NoEligibleAccount,
    SessionQuotaExhausted,
    SessionConcurrencyExhausted,
    CapacityUnknown,
    ReauthenticationRequired,
    Unsupported,
    SessionUncertain,
    OperationConflict,
    OperationAlreadyExists,
    InvalidTransition,
    InvalidInput,
    StorageUnavailable,
    CredentialUnavailable,
    UpstreamUnavailable,
}

#[derive(Clone, Debug, thiserror::Error, Serialize, Deserialize)]
#[error("{code:?}: {message}")]
pub struct Error {
    pub code: ErrorCode,
    /// Static/sanitized text only, never raw SQL, HTTP bodies or credential errors.
    pub message: String,
    pub binding_id: Option<BindingId>,
    pub attempt_id: Option<AttemptId>,
    pub binding_preserved: bool,
    pub request_state: DispatchCertainty,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            binding_id: None,
            attempt_id: None,
            binding_preserved: false,
            request_state: DispatchCertainty::NotDispatched,
        }
    }
    pub fn bound(mut self, binding: &BindingId) -> Self {
        self.binding_id = Some(binding.clone());
        self.binding_preserved = true;
        self
    }
    pub fn with_attempt(mut self, attempt: &AttemptId) -> Self {
        self.attempt_id = Some(attempt.clone());
        self
    }
}

pub type Result<T> = std::result::Result<T, Error>;
