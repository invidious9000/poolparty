//! Serialized refresh ownership with a durable fence before external token issuance.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::Mutex;

use crate::{
    codex_auth::{CodexAuth, CodexRefresher},
    config::StateDirectory,
    domain::*,
    ports::*,
};

#[async_trait]
impl CredentialRefresher for CodexRefresher {
    async fn refresh(&self, current: &SecretValue, now: Timestamp) -> Result<SecretValue> {
        CodexRefresher::refresh(self, current, now).await
    }
}

#[derive(Clone)]
pub struct CredentialMaintenance {
    store: Arc<dyn VersionedCredentialStore>,
    refresher: Arc<dyn CredentialRefresher>,
    state: Arc<StateDirectory>,
    ledger: Arc<dyn Ledger>,
    locks: BTreeMap<CredentialId, Arc<Mutex<()>>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRefresh {
    schema: u32,
    credential: CredentialId,
    generation: u64,
    identity_digest: String,
    access_digest: String,
}

impl CredentialMaintenance {
    pub fn new(
        store: Arc<dyn VersionedCredentialStore>,
        refresher: Arc<dyn CredentialRefresher>,
        state: Arc<StateDirectory>,
        ledger: Arc<dyn Ledger>,
        credentials: Vec<CredentialId>,
    ) -> Result<Self> {
        let mut locks = BTreeMap::new();
        for id in credentials {
            if locks.insert(id, Arc::new(Mutex::new(()))).is_some() {
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    "Duplicate managed credential.",
                ));
            }
        }
        Ok(Self {
            store,
            refresher,
            state,
            ledger,
            locks,
        })
    }

    pub async fn resolve(
        &self,
        id: &CredentialId,
        product: Product,
        expected_account: Option<&str>,
        now: Timestamp,
        force_refresh: bool,
    ) -> Result<CredentialRef> {
        // Own the entire operation, including lock and filesystem workers, even if
        // the requesting task disappears during external issuance or writeback.
        let owner = self.clone();
        let id = id.clone();
        let expected_account = expected_account.map(str::to_owned);
        tokio::spawn(async move {
            owner
                .resolve_inner(
                    &id,
                    product,
                    expected_account.as_deref(),
                    now,
                    force_refresh,
                )
                .await
        })
        .await
        .map_err(|_| unresolved())?
    }

    async fn resolve_inner(
        &self,
        id: &CredentialId,
        product: Product,
        expected_account: Option<&str>,
        now: Timestamp,
        force_refresh: bool,
    ) -> Result<CredentialRef> {
        let lock = self.locks.get(id).ok_or_else(|| {
            Error::new(
                ErrorCode::CredentialUnavailable,
                "Credential is not managed.",
            )
        })?;
        let _guard = lock.lock().await;
        let (reference, secret) = self.store.latest(id).await?;
        self.ledger
            .advance_credential_generation(&reference)
            .await?;
        if product != Product::CodexSubscription {
            if force_refresh {
                return Err(Error::new(
                    ErrorCode::Unsupported,
                    "API keys have no OAuth refresh operation.",
                ));
            }
            if secret.expose().is_empty() {
                return Err(Error::new(
                    ErrorCode::CredentialUnavailable,
                    "Credential is empty.",
                ));
            }
            return Ok(reference);
        }
        let auth = CodexAuth::parse(&secret)?;
        if expected_account.is_none_or(|expected| expected != auth.account_id()) {
            return Err(Error::new(
                ErrorCode::ReauthenticationRequired,
                "Credential does not match the explicitly enrolled upstream account.",
            ));
        }
        let path = self.marker_path(id);
        let current_identity = identity_digest(&auth);
        let current_access = access_digest(&secret)?;
        if read_marker(path.clone(), self.state.clone())
            .await?
            .is_some()
        {
            // A new token alone cannot prove that all writeback checks succeeded.
            // Pending refreshes need explicit reconciliation, including after restart.
            return Err(unresolved());
        }
        if !force_refresh && !auth.needs_refresh(now, 300_000)? {
            return Ok(reference);
        }
        let pending = PendingRefresh {
            schema: 1,
            credential: id.clone(),
            generation: reference.generation,
            identity_digest: current_identity,
            access_digest: current_access,
        };
        write_marker(path.clone(), pending, self.state.clone()).await?;
        // From this point every error leaves the marker. A later invocation cannot
        // blindly reissue a refresh using the old token after ambiguous issuance.
        let next = self.refresher.refresh(&secret, now).await?;
        let refreshed = CodexAuth::parse(&next)?;
        if identity_digest(&refreshed) != identity_digest(&auth) {
            return Err(Error::new(
                ErrorCode::ReauthenticationRequired,
                "Refreshed identity differs from the enrolled account.",
            ));
        }
        let usable = !refreshed.needs_refresh(now, 300_000)?;
        // Preserve a rotated refresh token even if its access token is already stale.
        let updated = self.store.replace(&reference, next).await?;
        self.ledger.advance_credential_generation(&updated).await?;
        if !usable {
            return Err(unresolved());
        }
        remove_marker(path, self.state.clone()).await?;
        Ok(updated)
    }

    fn marker_path(&self, id: &CredentialId) -> PathBuf {
        self.state
            .database
            .parent()
            .expect("state directory has a parent")
            .join(format!("refresh-{id}.pending.json"))
    }
}

fn identity_digest(auth: &CodexAuth) -> String {
    let mut hash = Sha256::new();
    hash.update(auth.account_id().as_bytes());
    hash.update([0]);
    if let Some(subject) = auth.subject() {
        hash.update(subject.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
fn access_digest(secret: &SecretValue) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(secret.expose()).map_err(|_| unresolved())?;
    let access = value
        .get("tokens")
        .and_then(|v| v.get("access_token"))
        .and_then(|v| v.as_str())
        .ok_or_else(unresolved)?;
    Ok(format!("{:x}", Sha256::digest(access.as_bytes())))
}
fn unresolved() -> Error {
    Error::new(
        ErrorCode::ReauthenticationRequired,
        "A prior credential refresh is unresolved; reconcile the stored bundle or re-enroll before retrying.",
    )
}
fn marker_error() -> Error {
    Error::new(
        ErrorCode::CredentialUnavailable,
        "Cannot durably maintain the credential refresh fence.",
    )
}

async fn read_marker(
    path: PathBuf,
    ownership: Arc<StateDirectory>,
) -> Result<Option<PendingRefresh>> {
    tokio::task::spawn_blocking(move || {
        let _ownership = ownership;
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(marker_error()),
        };
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|_| marker_error())?;
        if bytes.len() > 8192 {
            return Err(marker_error());
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| marker_error())
    })
    .await
    .map_err(|_| marker_error())?
}
async fn write_marker(
    path: PathBuf,
    marker: PendingRefresh,
    ownership: Arc<StateDirectory>,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let _ownership = ownership;
        let bytes = serde_json::to_vec(&marker).map_err(|_| marker_error())?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|_| marker_error())?;
        file.write_all(&bytes).map_err(|_| marker_error())?;
        file.sync_all().map_err(|_| marker_error())?;
        File::open(path.parent().ok_or_else(marker_error)?)
            .and_then(|f| f.sync_all())
            .map_err(|_| marker_error())
    })
    .await
    .map_err(|_| marker_error())?
}
async fn remove_marker(path: PathBuf, ownership: Arc<StateDirectory>) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let _ownership = ownership;
        std::fs::remove_file(&path).map_err(|_| marker_error())?;
        File::open(path.parent().ok_or_else(marker_error)?)
            .and_then(|f| f.sync_all())
            .map_err(|_| marker_error())
    })
    .await
    .map_err(|_| marker_error())?
}
