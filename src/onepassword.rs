//! 1Password CLI storage for credential-only items under exclusive writer ownership.
//!
//! The CLI offers no compare-and-set operation. The version checks here detect
//! many conflicts but cannot make independent writers safe. Hold process ownership
//! externally and prohibit external edits during rotation.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Stdio,
    time::Duration,
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
    versions: Mutex<BTreeMap<CredentialId, u64>>,
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
            versions: Mutex::new(BTreeMap::new()),
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
        versions: &mut BTreeMap<CredentialId, u64>,
    ) -> Result<(Value, u64, String)> {
        let item = self.invoke(mapping, None).await?;
        let (generation, value) = inspect(&item, mapping)?;
        if versions
            .get(&mapping.credential)
            .is_some_and(|previous| generation < *previous)
        {
            return Err(conflict());
        }
        versions.insert(mapping.credential.clone(), generation);
        Ok((item, generation, value))
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
        let mut versions = self.versions.lock().await;
        let (_, generation, value) = self.read(self.field(id)?, &mut versions).await?;
        Ok((
            CredentialRef {
                id: id.clone(),
                generation,
            },
            SecretValue::new(value),
        ))
    }
}

#[async_trait]
impl CredentialStore for OnePasswordStore {
    async fn load(&self, reference: &CredentialRef) -> Result<SecretValue> {
        let mut versions = self.versions.lock().await;
        let (_, generation, value) = self.read(self.field(&reference.id)?, &mut versions).await?;
        if generation != reference.generation {
            return Err(conflict());
        }
        Ok(SecretValue::new(value))
    }

    async fn replace(&self, expected: &CredentialRef, next: SecretValue) -> Result<CredentialRef> {
        if next.expose().is_empty() {
            return Err(failure());
        }
        let mut versions = self.versions.lock().await;
        let mapping = self.field(&expected.id)?;
        let (mut item, generation, _) = self.read(mapping, &mut versions).await?;
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
            .find(|field| field.get("id").and_then(Value::as_str) == Some(mapping.field.as_str()))
            .ok_or_else(failure)?;
        target
            .as_object_mut()
            .ok_or_else(failure)?
            .insert("value".into(), Value::String(next.expose().to_owned()));
        let expected_item = normalized(item.clone())?;
        let edited = self
            .invoke(
                mapping,
                Some(serde_json::to_vec(&item).map_err(|_| failure())?),
            )
            .await?;
        let (edited_generation, edited_value) = inspect(&edited, mapping)?;
        if edited_generation != next_generation
            || edited_value != next.expose()
            || normalized(edited)? != expected_item
        {
            return Err(conflict());
        }
        let (readback, readback_generation, readback_value) =
            self.read(mapping, &mut versions).await?;
        if readback_generation != next_generation
            || readback_value != next.expose()
            || normalized(readback)? != expected_item
        {
            return Err(conflict());
        }
        Ok(CredentialRef {
            id: expected.id.clone(),
            generation: next_generation,
        })
    }
}
