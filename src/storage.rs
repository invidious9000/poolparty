//! Durable affinity and admission. Transactions contain no asynchronous or network work.
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, Params, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{domain::*, ports::Ledger};

#[derive(Clone)]
pub struct SqliteLedger {
    connection: Arc<Mutex<Connection>>,
    ownership: Option<Arc<dyn Send + Sync>>,
}

fn unavailable() -> Error {
    Error::new(ErrorCode::StorageUnavailable, "ledger operation failed")
}
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorCode::InvalidInput, message)
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| unavailable())
}
fn decode<T: DeserializeOwned>(value: String) -> Result<T> {
    serde_json::from_str(&value).map_err(|_| unavailable())
}
fn get<T: DeserializeOwned>(db: &Connection, sql: &str, args: impl Params) -> Result<Option<T>> {
    db.query_row(sql, args, |row| row.get::<_, String>(0))
        .optional()
        .map_err(|_| unavailable())?
        .map(decode)
        .transpose()
}
fn all<T: DeserializeOwned>(db: &Connection, sql: &str, args: impl Params) -> Result<Vec<T>> {
    let mut statement = db.prepare(sql).map_err(|_| unavailable())?;
    let rows = statement
        .query_map(args, |row| row.get::<_, String>(0))
        .map_err(|_| unavailable())?;
    rows.map(|row| decode(row.map_err(|_| unavailable())?))
        .collect()
}
fn timestamp(now: Timestamp) -> Result<()> {
    if now < 0 {
        return Err(invalid("timestamp must be nonnegative"));
    }
    Ok(())
}

impl SqliteLedger {
    /// Open before accepting traffic. The caller holds exclusive daemon ownership.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path).map_err(|_| unavailable())?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|_| unavailable())?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS policies (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS observations (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS credentials (
                 id TEXT PRIMARY KEY, owner TEXT NOT NULL, product TEXT NOT NULL,
                 generation TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS credential_watermarks (
                 id TEXT PRIMARY KEY, generation TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS bindings (
                 id TEXT PRIMARY KEY, principal TEXT NOT NULL, session TEXT NOT NULL,
                 account TEXT NOT NULL REFERENCES accounts(id), data TEXT NOT NULL,
                 UNIQUE(principal, session)
             );
             CREATE TABLE IF NOT EXISTS attempts (
                 id TEXT PRIMARY KEY, binding TEXT NOT NULL REFERENCES bindings(id),
                 owner TEXT NOT NULL, operation TEXT, state TEXT NOT NULL, data TEXT NOT NULL,
                 UNIQUE(binding, operation)
             );
             CREATE INDEX IF NOT EXISTS attempts_owner_state ON attempts(owner, state);
             CREATE INDEX IF NOT EXISTS attempts_binding_state ON attempts(binding, state);",
            )
            .map_err(|_| unavailable())?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            ownership: None,
        })
    }

    /// Keep exclusive process ownership alive through blocking database work,
    /// including when its awaiting async task is cancelled during shutdown.
    pub fn with_ownership(mut self, ownership: Arc<dyn Send + Sync>) -> Self {
        self.ownership = Some(ownership);
        self
    }

    async fn run<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let ledger = self.clone();
        tokio::task::spawn_blocking(move || ledger.blocking(operation))
            .await
            .map_err(|_| unavailable())?
    }

    fn blocking<T>(&self, operation: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let _ownership = &self.ownership;
        let mut db = self.connection.lock().map_err(|_| unavailable())?;
        operation(&mut db)
    }
}

fn authorized_binding(db: &Connection, principal: &Principal, id: &BindingId) -> Result<Binding> {
    let binding: Binding = get(
        db,
        "SELECT data FROM bindings WHERE id=?1 AND principal=?2",
        params![id.as_str(), principal.id.as_str()],
    )?
    .ok_or_else(|| Error::new(ErrorCode::NotFound, "binding not found"))?;
    if !principal.pools.contains(&binding.intent.pool) {
        return Err(Error::new(ErrorCode::NotFound, "binding not found"));
    }
    Ok(binding)
}

fn credential_floor(db: &Connection, id: &CredentialId) -> Result<Option<u64>> {
    // Include existing metadata so databases created before watermarks were added
    // already have a floor before the first maintenance operation after upgrade.
    let mut statement = db.prepare(
        "SELECT generation FROM credential_watermarks WHERE id=?1 UNION ALL SELECT generation FROM credentials WHERE id=?1",
    ).map_err(|_| unavailable())?;
    let generations = statement
        .query_map([id.as_str()], |row| row.get::<_, String>(0))
        .map_err(|_| unavailable())?;
    let mut floor = None;
    for generation in generations {
        let generation = generation
            .map_err(|_| unavailable())?
            .parse::<u64>()
            .map_err(|_| unavailable())?;
        floor = Some(floor.map_or(generation, |previous: u64| previous.max(generation)));
    }
    Ok(floor)
}

fn advance_credential(db: &Connection, reference: &CredentialRef) -> Result<()> {
    if credential_floor(db, &reference.id)?
        .is_some_and(|generation| reference.generation < generation)
    {
        return Err(invalid("credential generation cannot decrease"));
    }
    db.execute(
        "INSERT INTO credential_watermarks(id,generation) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET generation=excluded.generation",
        params![reference.id.as_str(), reference.generation.to_string()],
    ).map_err(|_| unavailable())?;
    Ok(())
}

fn eligible(
    db: &Connection,
    account: &Account,
    intent: &CreateBinding,
    now: Timestamp,
) -> Result<()> {
    if !account.enabled
        || account.product != intent.product
        || !account.pools.contains(&intent.pool)
        || !account.models.contains(&intent.model)
    {
        return Err(Error::new(
            ErrorCode::NoEligibleAccount,
            "account does not satisfy session intent",
        ));
    }
    let policy: QuotaPolicy = get(
        db,
        "SELECT data FROM policies WHERE id=?1",
        [account.quota_owner.as_str()],
    )?
    .ok_or_else(|| Error::new(ErrorCode::CapacityUnknown, "quota policy is unavailable"))?;
    let observation: Option<UsageObservation> = get(
        db,
        "SELECT data FROM observations WHERE id=?1",
        [account.quota_owner.as_str()],
    )?;
    let known_available = match observation {
        Some(observation) => match observation.status {
            // Expiration never clears an observed failure. Only newer evidence can do so.
            CapacityStatus::Exhausted => {
                return Err(Error::new(
                    ErrorCode::SessionQuotaExhausted,
                    "account quota is exhausted",
                ));
            }
            CapacityStatus::ReauthenticationRequired => {
                return Err(Error::new(
                    ErrorCode::ReauthenticationRequired,
                    "account requires authentication",
                ));
            }
            CapacityStatus::Available => {
                observation.observed_at <= now && now < observation.valid_until
            }
            CapacityStatus::Unknown => false,
        },
        None => false,
    };
    if !known_available && policy.unknown == UnknownCapacityPolicy::Reject {
        return Err(Error::new(
            ErrorCode::CapacityUnknown,
            "account capacity is unknown or stale",
        ));
    }
    let pressure: u64 = db.query_row(
        "SELECT count(*) FROM attempts WHERE owner=?1 AND state IN ('reserved','dispatching','streaming','uncertain')",
        [account.quota_owner.as_str()], |row| row.get(0),
    ).map_err(|_| unavailable())?;
    if pressure >= u64::from(policy.max_concurrency) {
        return Err(Error::new(
            ErrorCode::SessionConcurrencyExhausted,
            "account concurrency is exhausted",
        ));
    }
    Ok(())
}

fn load_attempt(db: &Connection, id: &AttemptId) -> Result<Attempt> {
    get(db, "SELECT data FROM attempts WHERE id=?1", [id.as_str()])?
        .ok_or_else(|| Error::new(ErrorCode::NotFound, "attempt not found"))
}
fn state_name(state: AttemptState) -> &'static str {
    match state {
        AttemptState::Reserved => "reserved",
        AttemptState::Dispatching => "dispatching",
        AttemptState::Streaming => "streaming",
        AttemptState::Uncertain => "uncertain",
        AttemptState::Succeeded => "succeeded",
        AttemptState::Rejected => "rejected",
        AttemptState::NotDispatched => "not_dispatched",
    }
}
fn save_attempt(db: &Connection, attempt: &Attempt) -> Result<()> {
    db.execute(
        "UPDATE attempts SET state=?2, data=?3 WHERE id=?1",
        params![
            attempt.id.as_str(),
            state_name(attempt.state),
            encode(attempt)?
        ],
    )
    .map_err(|_| unavailable())?;
    Ok(())
}
fn advance(
    db: &Connection,
    id: &AttemptId,
    expected: AttemptState,
    next: AttemptState,
    now: Timestamp,
) -> Result<()> {
    let mut attempt = load_attempt(db, id)?;
    if attempt.state != expected || now < attempt.updated_at {
        return Err(Error::new(
            ErrorCode::InvalidTransition,
            "attempt transition is invalid",
        )
        .bound(&attempt.binding));
    }
    attempt.state = next;
    attempt.updated_at = now;
    save_attempt(db, &attempt)
}

#[async_trait]
impl Ledger for SqliteLedger {
    async fn accounts(&self, principal: &Principal) -> Result<Vec<Account>> {
        let principal = principal.clone();
        self.run(move |db| {
            let mut accounts: Vec<Account> = all(db, "SELECT data FROM accounts ORDER BY id", [])?;
            for account in &mut accounts {
                account.pools.retain(|pool| principal.pools.contains(pool));
            }
            accounts.retain(|account| !account.pools.is_empty());
            Ok(accounts)
        })
        .await
    }
    async fn set_account_enabled(&self, id: &AccountId, enabled: bool) -> Result<()> {
        let id = id.clone();
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            let mut account: Account =
                get(&tx, "SELECT data FROM accounts WHERE id=?1", [id.as_str()])?
                    .ok_or_else(|| Error::new(ErrorCode::NotFound, "account not found"))?;
            account.enabled = enabled;
            tx.execute(
                "UPDATE accounts SET data=?2 WHERE id=?1",
                params![id.as_str(), encode(&account)?],
            )
            .map_err(|_| unavailable())?;
            tx.commit().map_err(|_| unavailable())
        })
        .await
    }

    async fn usage_observation(&self, owner: &QuotaOwnerId) -> Result<Option<UsageObservation>> {
        let owner = owner.clone();
        self.run(move |db| {
            get(
                db,
                "SELECT data FROM observations WHERE id=?1",
                [owner.as_str()],
            )
        })
        .await
    }

    async fn advance_credential_generation(&self, reference: &CredentialRef) -> Result<()> {
        let reference = reference.clone();
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            advance_credential(&tx, &reference)?;
            tx.commit().map_err(|_| unavailable())
        })
        .await
    }

    async fn put_account(&self, account: Account) -> Result<()> {
        self.run(move |db| {
            if account.models.is_empty() || account.models.iter().any(|model| model.trim().is_empty()) {
                return Err(invalid("account must declare nonempty model names"));
            }
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| unavailable())?;
            if let Some(previous) = get::<Account>(&tx, "SELECT data FROM accounts WHERE id=?1", [account.id.as_str()])?
                && (previous.product != account.product || previous.quota_owner != account.quota_owner) {
                    return Err(invalid("account product and quota ownership are immutable"));
                }
            let previous: Option<(String, String, String)> = tx.query_row(
                "SELECT owner, product, generation FROM credentials WHERE id=?1", [account.credential.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional().map_err(|_| unavailable())?;
            let product = encode(&account.product)?;
            if let Some((owner, previous_product, generation)) = previous {
                if owner != account.quota_owner.as_str() || previous_product != product {
                    return Err(invalid("credential ownership is immutable"));
                }
                if account.credential.generation < generation.parse::<u64>().map_err(|_| unavailable())? {
                    return Err(invalid("credential generation cannot decrease"));
                }
            }
            advance_credential(&tx, &account.credential)?;
            tx.execute("INSERT INTO credentials(id, owner, product, generation) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET generation=excluded.generation",
                params![account.credential.id.as_str(), account.quota_owner.as_str(), product, account.credential.generation.to_string()]).map_err(|_| unavailable())?;
            tx.execute("INSERT INTO accounts(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
                params![account.id.as_str(), encode(&account)?]).map_err(|_| unavailable())?;
            tx.commit().map_err(|_| unavailable())
        }).await
    }

    async fn put_quota_policy(&self, policy: QuotaPolicy) -> Result<()> {
        if policy.max_concurrency == 0 {
            return Err(invalid("concurrency cap must be positive"));
        }
        self.run(move |db| {
            db.execute("INSERT INTO policies(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
                params![policy.owner.as_str(), encode(&policy)?]).map_err(|_| unavailable())?;
            Ok(())
        }).await
    }

    async fn observe(&self, observation: UsageObservation) -> Result<()> {
        timestamp(observation.observed_at)?;
        if observation.valid_until <= observation.observed_at {
            return Err(invalid("observation expiry must follow observation time"));
        }
        self.run(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| unavailable())?;
            if let Some(previous) = get::<UsageObservation>(&tx, "SELECT data FROM observations WHERE id=?1", [observation.owner.as_str()])? {
                if observation == previous { return Ok(()); }
                if observation.observed_at <= previous.observed_at { return Err(invalid("observation does not advance evidence time")); }
            }
            tx.execute("INSERT INTO observations(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
                params![observation.owner.as_str(), encode(&observation)?]).map_err(|_| unavailable())?;
            tx.commit().map_err(|_| unavailable())
        }).await
    }

    async fn create_binding(
        &self,
        principal: &Principal,
        intent: CreateBinding,
        now: Timestamp,
    ) -> Result<Binding> {
        timestamp(now)?;
        if !principal.pools.contains(&intent.pool) {
            return Err(Error::new(
                ErrorCode::Unauthorized,
                "pool is not authorized",
            ));
        }
        if intent.model.trim().is_empty()
            || intent
                .effort
                .as_ref()
                .is_some_and(|effort| effort.trim().is_empty())
        {
            return Err(invalid("model and effort must be nonempty when supplied"));
        }
        let principal = principal.clone();
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            if let Some(binding) = get::<Binding>(
                &tx,
                "SELECT data FROM bindings WHERE principal=?1 AND session=?2",
                params![principal.id.as_str(), intent.session.as_str()],
            )? {
                if !principal.pools.contains(&binding.intent.pool) {
                    return Err(Error::new(ErrorCode::NotFound, "binding not found"));
                }
                if binding.closed_at.is_some() {
                    return Err(
                        Error::new(ErrorCode::Closed, "binding is closed").bound(&binding.id)
                    );
                }
                if binding.intent != intent {
                    return Err(Error::new(
                        ErrorCode::IntentConflict,
                        "session intent is immutable",
                    )
                    .bound(&binding.id));
                }
                return Ok(binding);
            }
            let candidates: Vec<Account> = all(&tx, "SELECT data FROM accounts ORDER BY id", [])?;
            let mut selected = None;
            for account in candidates {
                if intent.account.as_ref().is_some_and(|id| id != &account.id) {
                    continue;
                }
                match eligible(&tx, &account, &intent, now) {
                    Ok(()) => {
                        selected = Some(account);
                        break;
                    }
                    Err(error) if error.code == ErrorCode::StorageUnavailable => return Err(error),
                    Err(_) => (),
                }
            }
            let account = selected
                .ok_or_else(|| Error::new(ErrorCode::NoEligibleAccount, "no eligible account"))?;
            let binding = Binding {
                id: BindingId::new(Uuid::new_v4().to_string()).map_err(|_| unavailable())?,
                principal: principal.id,
                intent,
                account: account.id,
                created_at: now,
                closed_at: None,
            };
            tx.execute(
                "INSERT INTO bindings(id,principal,session,account,data) VALUES(?1,?2,?3,?4,?5)",
                params![
                    binding.id.as_str(),
                    binding.principal.as_str(),
                    binding.intent.session.as_str(),
                    binding.account.as_str(),
                    encode(&binding)?
                ],
            )
            .map_err(|_| unavailable())?;
            tx.commit().map_err(|_| unavailable())?;
            Ok(binding)
        })
        .await
    }

    async fn binding(&self, principal: &Principal, id: &BindingId) -> Result<Binding> {
        let principal = principal.clone();
        let id = id.clone();
        self.run(move |db| authorized_binding(db, &principal, &id))
            .await
    }

    async fn close_binding(
        &self,
        principal: &Principal,
        id: &BindingId,
        now: Timestamp,
    ) -> Result<Binding> {
        timestamp(now)?;
        let principal = principal.clone();
        let id = id.clone();
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            let mut binding = authorized_binding(&tx, &principal, &id)?;
            if now < binding.created_at {
                return Err(invalid("close time precedes binding creation"));
            }
            if binding.closed_at.is_none() {
                binding.closed_at = Some(now);
                tx.execute(
                    "UPDATE bindings SET data=?2 WHERE id=?1",
                    params![id.as_str(), encode(&binding)?],
                )
                .map_err(|_| unavailable())?;
            }
            tx.commit().map_err(|_| unavailable())?;
            Ok(binding)
        })
        .await
    }

    async fn admit(
        &self,
        principal: &Principal,
        admission: Admission,
        now: Timestamp,
    ) -> Result<PreparedAttempt> {
        timestamp(now)?;
        if admission.request_fingerprint.is_empty() || admission.request_fingerprint.len() > 128 {
            return Err(invalid(
                "request fingerprint must contain 1..128 characters",
            ));
        }
        let principal = principal.clone();
        self.run(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| unavailable())?;
            let binding = authorized_binding(&tx, &principal, &admission.binding)?;
            if binding.closed_at.is_some() { return Err(Error::new(ErrorCode::Closed, "binding is closed").bound(&binding.id)); }
            if now < binding.created_at { return Err(invalid("admission precedes binding creation").bound(&binding.id)); }
            if binding.intent.model != admission.model || binding.intent.effort != admission.effort {
                return Err(Error::new(ErrorCode::IntentConflict, "request conflicts with bound intent").bound(&binding.id));
            }
            if let Some(operation) = &admission.operation
                && let Some(previous) = get::<Attempt>(&tx, "SELECT data FROM attempts WHERE binding=?1 AND operation=?2", params![binding.id.as_str(), operation.as_str()])? {
                    let code = if previous.request_fingerprint == admission.request_fingerprint { ErrorCode::OperationAlreadyExists } else { ErrorCode::OperationConflict };
                    let mut error = Error::new(code, "operation ID already exists").bound(&binding.id);
                    error.request_state = previous.state.certainty();
                    error.attempt_id = Some(previous.id);
                    return Err(error);
                }
            if let Some(uncertain) = get::<Attempt>(&tx, "SELECT data FROM attempts WHERE binding=?1 AND state='uncertain' ORDER BY id LIMIT 1", [binding.id.as_str()])? {
                let mut error = Error::new(ErrorCode::SessionUncertain, "session has unresolved dispatch").bound(&binding.id);
                error.attempt_id = Some(uncertain.id);
                return Err(error);
            }
            // Native clients without operation IDs cannot distinguish concurrent
            // turns from retries. Keep one such claim exclusive to its binding.
            if let Some(active) = get::<Attempt>(&tx,
                "SELECT data FROM attempts WHERE binding=?1 AND state IN ('reserved','dispatching','streaming') AND (?2 OR operation IS NULL) ORDER BY id LIMIT 1",
                params![binding.id.as_str(), admission.operation.is_none()],
            )? {
                let mut error = Error::new(ErrorCode::SessionConcurrencyExhausted, "session has an active request without explicit operation isolation").bound(&binding.id);
                error.attempt_id = Some(active.id);
                return Err(error);
            }
            let account: Account = get(&tx, "SELECT data FROM accounts WHERE id=?1", [binding.account.as_str()])?.ok_or_else(unavailable)?;
            eligible(&tx, &account, &binding.intent, now).map_err(|error| error.bound(&binding.id))?;
            if credential_floor(&tx, &account.credential.id)? != Some(account.credential.generation) {
                return Err(Error::new(ErrorCode::CredentialUnavailable, "account credential reference is stale").bound(&binding.id));
            }
            let attempt = Attempt {
                id: AttemptId::new(Uuid::new_v4().to_string()).map_err(|_| unavailable())?,
                binding: binding.id.clone(), quota_owner: account.quota_owner.clone(), operation: admission.operation,
                request_fingerprint: admission.request_fingerprint, credential: account.credential.clone(),
                state: AttemptState::Reserved, created_at: now, updated_at: now,
            };
            tx.execute("INSERT INTO attempts(id,binding,owner,operation,state,data) VALUES(?1,?2,?3,?4,?5,?6)",
                params![attempt.id.as_str(), binding.id.as_str(), attempt.quota_owner.as_str(), attempt.operation.as_ref().map(OperationId::as_str), state_name(attempt.state), encode(&attempt)?]).map_err(|_| unavailable())?;
            tx.commit().map_err(|_| unavailable())?;
            Ok(PreparedAttempt { attempt, binding, account })
        }).await
    }

    async fn mark_dispatching(&self, id: &AttemptId, now: Timestamp) -> Result<()> {
        timestamp(now)?;
        let id = id.clone();
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            advance(
                &tx,
                &id,
                AttemptState::Reserved,
                AttemptState::Dispatching,
                now,
            )?;
            tx.commit().map_err(|_| unavailable())
        })
        .await
    }

    async fn mark_streaming(&self, id: &AttemptId, now: Timestamp) -> Result<()> {
        timestamp(now)?;
        let id = id.clone();
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            advance(
                &tx,
                &id,
                AttemptState::Dispatching,
                AttemptState::Streaming,
                now,
            )?;
            tx.commit().map_err(|_| unavailable())
        })
        .await
    }

    async fn settle(&self, id: &AttemptId, outcome: Settlement, now: Timestamp) -> Result<Attempt> {
        timestamp(now)?;
        let id = id.clone();
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            let mut attempt = load_attempt(&tx, &id)?;
            let next = match outcome {
                Settlement::Succeeded => AttemptState::Succeeded,
                Settlement::Rejected => AttemptState::Rejected,
                Settlement::NotDispatched => AttemptState::NotDispatched,
                Settlement::Uncertain => AttemptState::Uncertain,
            };
            if attempt.state == next {
                return Ok(attempt);
            }
            let allowed = match attempt.state {
                AttemptState::Reserved => next == AttemptState::NotDispatched,
                AttemptState::Dispatching => true,
                AttemptState::Streaming | AttemptState::Uncertain => {
                    next != AttemptState::NotDispatched
                }
                AttemptState::Succeeded | AttemptState::Rejected | AttemptState::NotDispatched => {
                    false
                }
            };
            if !allowed || now < attempt.updated_at {
                return Err(Error::new(
                    ErrorCode::InvalidTransition,
                    "attempt settlement is invalid",
                )
                .bound(&attempt.binding));
            }
            attempt.state = next;
            attempt.updated_at = now;
            save_attempt(&tx, &attempt)?;
            tx.commit().map_err(|_| unavailable())?;
            Ok(attempt)
        })
        .await
    }

    async fn attempt(&self, principal: &Principal, id: &AttemptId) -> Result<Attempt> {
        let principal = principal.clone();
        let id = id.clone();
        self.run(move |db| {
            let attempt = load_attempt(db, &id)?;
            authorized_binding(db, &principal, &attempt.binding).map_err(|error| {
                if error.code == ErrorCode::NotFound {
                    Error::new(ErrorCode::NotFound, "attempt not found")
                } else {
                    error
                }
            })?;
            Ok(attempt)
        })
        .await
    }

    async fn recover(&self, now: Timestamp) -> Result<()> {
        timestamp(now)?;
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| unavailable())?;
            let attempts: Vec<Attempt> = all(
                &tx,
                "SELECT data FROM attempts WHERE state IN ('reserved','dispatching','streaming')",
                [],
            )?;
            for mut attempt in attempts {
                attempt.state = if attempt.state == AttemptState::Reserved {
                    AttemptState::NotDispatched
                } else {
                    AttemptState::Uncertain
                };
                attempt.updated_at = now.max(attempt.updated_at);
                save_attempt(&tx, &attempt)?;
            }
            tx.commit().map_err(|_| unavailable())
        })
        .await
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::config::StateDirectory;

    #[tokio::test]
    async fn cancelled_waiter_does_not_release_state_lock_before_blocking_sql_finishes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap().join("state");
        let state = Arc::new(StateDirectory::acquire(&path).unwrap());
        let weak = Arc::downgrade(&state);
        let ledger = SqliteLedger::open(&state.database)
            .unwrap()
            .with_ownership(state.clone());
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker = ledger.clone();
        let waiter = tokio::spawn(async move {
            worker
                .run(move |db| {
                    db.execute_batch("CREATE TABLE ownership_probe (value INTEGER NOT NULL)")
                        .map_err(|_| unavailable())?;
                    let _ = entered.send(());
                    released.recv().map_err(|_| unavailable())?;
                    db.execute("INSERT INTO ownership_probe(value) VALUES(1)", [])
                        .map_err(|_| unavailable())?;
                    Ok(())
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        drop(ledger);
        drop(state);
        assert!(weak.upgrade().is_some());
        assert!(StateDirectory::acquire(&path).is_err());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let reopened_state = StateDirectory::acquire(&path).unwrap();
        let connection = Connection::open(&reopened_state.database).unwrap();
        let rows: u64 = connection
            .query_row("SELECT count(*) FROM ownership_probe", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }
}
