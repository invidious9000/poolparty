//! Synthetic demo support. These credentials and observations never contact a provider.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Mutex,
};

use async_trait::async_trait;
use fs2::FileExt;

use crate::{domain::*, ports::*};

/// Test/demo implementation only. Durable production secret backends are separate adapters.
#[derive(Default)]
pub struct MemoryCredentials {
    values: Mutex<BTreeMap<CredentialId, (u64, SecretValue)>>,
}

impl MemoryCredentials {
    pub fn new(entries: Vec<(CredentialRef, SecretValue)>) -> Result<Self> {
        let mut values = BTreeMap::new();
        for (reference, value) in entries {
            if values
                .insert(reference.id, (reference.generation, value))
                .is_some()
            {
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    "Duplicate credential ID.",
                ));
            }
        }
        Ok(Self {
            values: Mutex::new(values),
        })
    }
}

#[async_trait]
impl CredentialStore for MemoryCredentials {
    async fn load(&self, reference: &CredentialRef) -> Result<SecretValue> {
        let values = self.values.lock().map_err(|_| credential_error())?;
        match values.get(&reference.id) {
            Some((generation, value)) if *generation == reference.generation => {
                Ok(SecretValue::new(value.expose().into()))
            }
            _ => Err(credential_error()),
        }
    }
    async fn replace(&self, expected: &CredentialRef, next: SecretValue) -> Result<CredentialRef> {
        let mut values = self.values.lock().map_err(|_| credential_error())?;
        let (generation, value) = values.get_mut(&expected.id).ok_or_else(credential_error)?;
        if *generation != expected.generation {
            return Err(credential_error());
        }
        let next_generation = generation.checked_add(1).ok_or_else(credential_error)?;
        *generation = next_generation;
        *value = next;
        Ok(CredentialRef {
            id: expected.id.clone(),
            generation: next_generation,
        })
    }
}
fn credential_error() -> Error {
    Error::new(
        ErrorCode::CredentialUnavailable,
        "Credential generation is unavailable.",
    )
}

/// Held for the daemon lifetime. A second process cannot run startup recovery on
/// a database while its first owner is still streaming upstream requests.
pub struct StateDirectory {
    _lock: File,
    pub database: PathBuf,
}
impl StateDirectory {
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path).map_err(|_| state_error())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(path)
                .map_err(|_| state_error())?
                .permissions()
                .mode()
                & 0o077
                != 0
            {
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    "State directory must be private to its owner (mode 0700).",
                ));
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(path.join("daemon.lock"))
            .map_err(|_| state_error())?;
        lock.try_lock_exclusive().map_err(|_| {
            Error::new(
                ErrorCode::StorageUnavailable,
                "State directory already has an active owner.",
            )
        })?;
        Ok(Self {
            _lock: lock,
            database: path.join("poolparty.sqlite3"),
        })
    }
}
fn state_error() -> Error {
    Error::new(
        ErrorCode::StorageUnavailable,
        "Cannot open private state directory.",
    )
}

pub async fn seed_demo(
    ledger: &dyn Ledger,
    now: Timestamp,
) -> Result<(Principal, MemoryCredentials)> {
    let pool = PoolId::new("demo").expect("constant ID");
    let mut secrets = Vec::new();
    for label in ["a", "b"] {
        let owner = QuotaOwnerId::new(format!("owner-{label}")).expect("constant ID");
        let credential = CredentialRef {
            id: CredentialId::new(format!("credential-{label}")).expect("constant ID"),
            generation: 1,
        };
        ledger
            .put_account(Account {
                id: AccountId::new(format!("account-{label}")).expect("constant ID"),
                product: Product::CodexSubscription,
                quota_owner: owner.clone(),
                pools: BTreeSet::from([pool.clone()]),
                credential: credential.clone(),
                models: BTreeSet::from(["synthetic-model".into()]),
                enabled: true,
            })
            .await?;
        ledger
            .put_quota_policy(QuotaPolicy {
                owner: owner.clone(),
                max_concurrency: 1,
                unknown: UnknownCapacityPolicy::Reject,
            })
            .await?;
        ledger
            .observe(UsageObservation {
                owner,
                observed_at: now,
                valid_until: now.saturating_add(86_400_000),
                status: CapacityStatus::Available,
                windows: Vec::new(),
                balances: Vec::new(),
                source: "synthetic-demo".into(),
            })
            .await?;
        secrets.push((
            credential,
            SecretValue::new("synthetic-no-provider-access".into()),
        ));
    }
    Ok((
        Principal {
            id: PrincipalId::new("demo-client").expect("constant ID"),
            pools: BTreeSet::from([pool]),
        },
        MemoryCredentials::new(secrets)?,
    ))
}
