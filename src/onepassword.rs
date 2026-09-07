//! 1Password CLI storage for credential-only items under exclusive writer ownership.
//!
//! The CLI offers no compare-and-set operation. The version checks here detect
//! many conflicts but cannot make independent writers safe. Hold process ownership
//! externally and prohibit external edits during rotation.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Mutex,
};

use crate::{
    domain::*,
    ports::{CredentialStore, SecretValue, VersionedCredentialStore},
};

const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
const READ_FAILURE_BACKOFF: Duration = Duration::from_secs(15 * 60);

struct CachedCredential {
    generation: u64,
    value: SecretValue,
    expires_at: Instant,
}

#[derive(Default)]
struct StoreState {
    versions: BTreeMap<CredentialId, u64>,
    cache: BTreeMap<CredentialId, CachedCredential>,
    blocked_until: Option<Instant>,
    credential_blocked_until: BTreeMap<CredentialId, Instant>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultField {
    pub credential: CredentialId,
    pub vault: String,
    pub item: String,
    pub field: String,
}

pub struct OnePasswordStore {
    fields: BTreeMap<CredentialId, VaultField>,
    service_token: SecretValue,
    executable: PathBuf,
    timeout: Duration,
    state: Mutex<StoreState>,
    cache_ttl: Option<Duration>,
    cache_clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

fn failure() -> Error {
    Error::new(
        ErrorCode::CredentialUnavailable,
        "credential store operation failed; inspect current generation before retrying",
    )
}
fn conflict() -> Error {
    Error::new(
        ErrorCode::CredentialUnavailable,
        "credential item changed or failed preservation checks; reconcile before retrying",
    )
}
fn valid_reference(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

impl OnePasswordStore {
    pub fn new(
        fields: Vec<VaultField>,
        service_token: SecretValue,
        executable: PathBuf,
    ) -> Result<Self> {
        if service_token.expose().is_empty() || !executable.is_absolute() {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "a service token and absolute trusted executable path are required",
            ));
        }
        let mut mapping = BTreeMap::new();
        let mut items = BTreeSet::new();
        for field in fields {
            if !valid_reference(&field.vault)
                || !valid_reference(&field.item)
                || !valid_reference(&field.field)
                || !items.insert((field.vault.clone(), field.item.clone()))
                || mapping.insert(field.credential.clone(), field).is_some()
            {
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    "credential mappings require valid stable IDs, unique credentials and one credential per item",
                ));
            }
        }
        Ok(Self {
            fields: mapping,
            service_token,
            executable,
            timeout: Duration::from_secs(20),
            state: Mutex::new(StoreState::default()),
            cache_ttl: None,
            cache_clock: Arc::new(Instant::now),
        })
    }

    /// Bound each CLI subprocess. A timeout during edit leaves write outcome unknown.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(120) {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "CLI timeout must be greater than zero and at most 120 seconds",
            ));
        }
        self.timeout = timeout;
        Ok(self)
    }

    /// Opt into process-only read caching and a global vault failure backoff.
    /// Uncached construction retains fresh CLI reads for one-shot maintenance.
    pub fn with_read_cache(mut self, ttl: Duration) -> Result<Self> {
        if ttl.is_zero() || ttl > Duration::from_secs(3600) {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "credential read cache TTL must be greater than zero and at most one hour",
            ));
        }
        self.cache_ttl = Some(ttl);
        Ok(self)
    }

    /// Inject a monotonic clock before sharing the store, for deterministic tests.
    pub fn with_cache_clock(mut self, clock: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        self.cache_clock = clock;
        self
    }

    fn check_backoff(&self, state: &StoreState, id: &CredentialId) -> Result<()> {
        if state
            .blocked_until
            .is_some_and(|until| (self.cache_clock)() < until)
            || state
                .credential_blocked_until
                .get(id)
                .is_some_and(|until| (self.cache_clock)() < *until)
        {
            return Err(failure());
        }
        Ok(())
    }

    fn record_failure(&self, state: &mut StoreState, id: &CredentialId, global: bool) {
        state.cache.remove(id);
        if self.cache_ttl.is_some()
            && let Some(until) = (self.cache_clock)().checked_add(READ_FAILURE_BACKOFF)
        {
            if global {
                state.blocked_until = Some(until);
            } else {
                state.credential_blocked_until.insert(id.clone(), until);
            }
        }
    }

    fn cache_value(&self, state: &mut StoreState, id: &CredentialId, generation: u64, value: &str) {
        if let Some(expires_at) = self
            .cache_ttl
            .and_then(|ttl| (self.cache_clock)().checked_add(ttl))
        {
            state.cache.insert(
                id.clone(),
                CachedCredential {
                    generation,
                    value: SecretValue::new(value.to_owned()),
                    expires_at,
                },
            );
        }
    }

    async fn cached_or_read(
        &self,
        mapping: &VaultField,
        state: &mut StoreState,
    ) -> Result<(u64, SecretValue)> {
        // A known vault outage must also stop cached expiring credentials from
        // initiating an OAuth exchange whose writeback is already known to fail.
        self.check_backoff(state, &mapping.credential)?;
        if let Some(cached) = state.cache.get(&mapping.credential)
            && state.blocked_until.is_none()
            && (self.cache_clock)() < cached.expires_at
            && state.versions.get(&mapping.credential) == Some(&cached.generation)
        {
            return Ok((
                cached.generation,
                SecretValue::new(cached.value.expose().to_owned()),
            ));
        }
        let (_, generation, value) = self.read(mapping, state).await?;
        self.cache_value(state, &mapping.credential, generation, &value);
        Ok((generation, SecretValue::new(value)))
    }

    pub async fn latest(&self, id: &CredentialId) -> Result<(CredentialRef, SecretValue)> {
        <Self as VersionedCredentialStore>::latest(self, id).await
    }

    fn field(&self, id: &CredentialId) -> Result<&VaultField> {
        self.fields.get(id).ok_or_else(failure)
    }

    async fn invoke(&self, mapping: &VaultField, edit: Option<Vec<u8>>) -> Result<Value> {
        if edit
            .as_ref()
            .is_some_and(|bytes| bytes.len() > OUTPUT_LIMIT)
        {
            return Err(failure());
        }
        // op requires a config location even with service-account authentication.
        // Never resolve it from the operator's HOME or enable a caching daemon
        // that could outlive this invocation's private directory.
        let mut config_builder = tempfile::Builder::new();
        config_builder.prefix("poolparty-op-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            config_builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let config_dir = config_builder.tempdir().map_err(|_| failure())?;
        let mut command = Command::new(&self.executable);
        command
            .args([
                "item",
                if edit.is_some() { "edit" } else { "get" },
                mapping.item.as_str(),
                "--vault",
                mapping.vault.as_str(),
                "--format",
                "json",
                "--reveal",
            ])
            .arg("--config")
            .arg(config_dir.path())
            .arg("--cache=false")
            .env_clear()
            .env("OP_SERVICE_ACCOUNT_TOKEN", self.service_token.expose())
            .stdin(if edit.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // PATH permits the trusted executable's normal helpers. No inherited OP_*
        // settings, proxy credentials, provider tokens or shell hooks cross here.
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        let mut child = command.spawn().map_err(|_| failure())?;
        let stdout = child.stdout.take().ok_or_else(failure)?;
        let stderr = child.stderr.take().ok_or_else(failure)?;
        let stdin = child.stdin.take();
        let result = tokio::time::timeout(self.timeout, async {
            let (stdout, _, (), status) = tokio::try_join!(
                read_bounded(stdout, OUTPUT_LIMIT),
                read_bounded(stderr, STDERR_LIMIT),
                async move {
                    if let (Some(mut stdin), Some(bytes)) = (stdin, edit) {
                        stdin.write_all(&bytes).await.map_err(|_| failure())?;
                        stdin.shutdown().await.map_err(|_| failure())?;
                    }
                    Ok::<(), Error>(())
                },
                async { child.wait().await.map_err(|_| failure()) },
            )?;
            if !status.success() {
                return Err(failure());
            }
            serde_json::from_slice(&stdout).map_err(|_| failure())
        })
        .await;
        match result {
            Ok(Ok(value)) => Ok(value),
            _ => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(failure())
            }
        }
    }

    async fn read(
        &self,
        mapping: &VaultField,
        state: &mut StoreState,
    ) -> Result<(Value, u64, String)> {
        self.check_backoff(state, &mapping.credential)?;
        state.cache.remove(&mapping.credential);
        let item = match self.invoke(mapping, None).await {
            Ok(item) => item,
            Err(error) => {
                self.record_failure(state, &mapping.credential, true);
                return Err(error);
            }
        };
        // A successful CLI call proves service recovery independently of whether
        // this particular credential item's shape/generation is usable.
        state.blocked_until = None;
        let result = (|| {
            let (generation, value) = inspect(&item, mapping)?;
            if state
                .versions
                .get(&mapping.credential)
                .is_some_and(|previous| generation < *previous)
            {
                return Err(conflict());
            }
            state
                .versions
                .insert(mapping.credential.clone(), generation);
            Ok((item, generation, value))
        })();
        if result.is_err() {
            self.record_failure(state, &mapping.credential, false);
        } else {
            state.credential_blocked_until.remove(&mapping.credential);
        }
        result
    }
}

async fn read_bounded(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut buffer)
        .await
        .map_err(|_| failure())?;
    if buffer.len() > limit {
        return Err(failure());
    }
    Ok(buffer)
}

fn inspect(item: &Value, mapping: &VaultField) -> Result<(u64, String)> {
    if item.get("id").and_then(Value::as_str) != Some(mapping.item.as_str())
        || item.pointer("/vault/id").and_then(Value::as_str) != Some(mapping.vault.as_str())
    {
        return Err(conflict());
    }
    let version = item
        .get("version")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(failure)?;
    let fields = item
        .get("fields")
        .and_then(Value::as_array)
        .ok_or_else(failure)?;
    let mut ids = BTreeSet::new();
    let mut target = None;
    for field in fields {
        let id = field
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(failure)?;
        if !ids.insert(id) {
            return Err(conflict());
        }
        if id == mapping.field {
            if !matches!(
                field.get("type").and_then(Value::as_str),
                Some("STRING" | "CONCEALED")
            ) {
                return Err(Error::new(
                    ErrorCode::Unsupported,
                    "credential field must be a string or concealed field",
                ));
            }
            target = Some(
                field
                    .get("value")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(failure)?
                    .to_owned(),
            );
        }
    }
    Ok((version, target.ok_or_else(failure)?))
}

fn empty_date(field: &Value) -> bool {
    field.get("type").and_then(Value::as_str) == Some("DATE")
        && field
            .get("value")
            .is_none_or(|value| value.is_null() || value.as_str() == Some(""))
}

fn unsupported_content(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            key.contains("passkey")
                || key == "files" && value.as_array().is_none_or(|files| !files.is_empty())
                || key == "type"
                    && value.as_str().is_some_and(|value| {
                        value.eq_ignore_ascii_case("file") || value.eq_ignore_ascii_case("passkey")
                    })
                || unsupported_content(value)
        }),
        Value::Array(array) => array.iter().any(unsupported_content),
        _ => false,
    }
}

fn normalized(mut item: Value) -> Result<Value> {
    let object = item.as_object_mut().ok_or_else(failure)?;
    object.remove("version");
    object.remove("updated_at");
    // Server-owned audit metadata changes when the service account edits an item.
    object.remove("last_edited_by");
    let fields = object
        .get_mut("fields")
        .and_then(Value::as_array_mut)
        .ok_or_else(failure)?;
    fields.retain(|field| !empty_date(field));
    for field in fields.iter_mut() {
        if field.get("value").is_none_or(Value::is_null) {
            field
                .as_object_mut()
                .ok_or_else(failure)?
                .insert("value".into(), Value::String(String::new()));
        }
    }
    fields.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .cmp(&right.get("id").and_then(Value::as_str))
    });
    if let Some(sections) = object.get_mut("sections").and_then(Value::as_array_mut) {
        sections.sort_by(|left, right| {
            left.get("id")
                .and_then(Value::as_str)
                .cmp(&right.get("id").and_then(Value::as_str))
        });
    }
    Ok(item)
}

#[async_trait]
impl VersionedCredentialStore for OnePasswordStore {
    async fn latest(&self, id: &CredentialId) -> Result<(CredentialRef, SecretValue)> {
        let mut state = self.state.lock().await;
        let (generation, value) = self.cached_or_read(self.field(id)?, &mut state).await?;
        Ok((
            CredentialRef {
                id: id.clone(),
                generation,
            },
            value,
        ))
    }
}

#[async_trait]
impl CredentialStore for OnePasswordStore {
    async fn load(&self, reference: &CredentialRef) -> Result<SecretValue> {
        let mut state = self.state.lock().await;
        if self.cache_ttl.is_some()
            && state
                .versions
                .get(&reference.id)
                .is_some_and(|generation| reference.generation < *generation)
        {
            return Err(conflict());
        }
        if state
            .cache
            .get(&reference.id)
            .is_some_and(|cached| cached.generation != reference.generation)
        {
            state.cache.remove(&reference.id);
        }
        let (generation, value) = self
            .cached_or_read(self.field(&reference.id)?, &mut state)
            .await?;
        if generation != reference.generation {
            state.cache.remove(&reference.id);
            return Err(conflict());
        }
        Ok(value)
    }

    async fn replace(&self, expected: &CredentialRef, next: SecretValue) -> Result<CredentialRef> {
        if next.expose().is_empty() {
            return Err(failure());
        }
        let mut state = self.state.lock().await;
        state.cache.remove(&expected.id);
        self.check_backoff(&state, &expected.id)?;
        let mapping = self.field(&expected.id)?;
        let result = async {
            let (mut item, generation, _) = self.read(mapping, &mut state).await?;
            if generation != expected.generation {
                return Err(conflict());
            }
            if unsupported_content(&item) {
                return Err(Error::new(
                    ErrorCode::Unsupported,
                    "credential item contains content unsupported by JSON updates",
                ));
            }
            let next_generation = generation.checked_add(1).ok_or_else(conflict)?;
            let fields = item
                .get_mut("fields")
                .and_then(Value::as_array_mut)
                .ok_or_else(failure)?;
            // Empty DATE fields can otherwise become zero dates during a CLI round trip.
            fields.retain(|field| !empty_date(field));
            let target = fields
                .iter_mut()
                .find(|field| {
                    field.get("id").and_then(Value::as_str) == Some(mapping.field.as_str())
                })
                .ok_or_else(failure)?;
            target
                .as_object_mut()
                .ok_or_else(failure)?
                .insert("value".into(), Value::String(next.expose().to_owned()));
            let expected_item = normalized(item.clone())?;
            let edited = match self
                .invoke(
                    mapping,
                    Some(serde_json::to_vec(&item).map_err(|_| failure())?),
                )
                .await
            {
                Ok(edited) => edited,
                Err(error) => {
                    self.record_failure(&mut state, &expected.id, true);
                    return Err(error);
                }
            };
            let (edited_generation, edited_value) = inspect(&edited, mapping)?;
            state
                .versions
                .entry(expected.id.clone())
                .and_modify(|generation| *generation = (*generation).max(edited_generation))
                .or_insert(edited_generation);
            if edited_generation != next_generation
                || edited_value != next.expose()
                || normalized(edited)? != expected_item
            {
                return Err(conflict());
            }
            let (readback, readback_generation, readback_value) =
                self.read(mapping, &mut state).await?;
            if readback_generation != next_generation
                || readback_value != next.expose()
                || normalized(readback)? != expected_item
            {
                return Err(conflict());
            }
            self.cache_value(&mut state, &expected.id, next_generation, &readback_value);
            Ok(CredentialRef {
                id: expected.id.clone(),
                generation: next_generation,
            })
        }
        .await;
        if result.is_err() {
            self.record_failure(&mut state, &expected.id, false);
        }
        result
    }
}
