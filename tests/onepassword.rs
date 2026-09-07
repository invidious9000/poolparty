#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

use poolparty::{
    domain::*,
    onepassword::{OnePasswordStore, VaultField},
    ports::{CredentialStore, SecretValue},
};
use serde_json::{Value, json};
use tempfile::TempDir;

// This executable and its files contain synthetic fixtures only. It never invokes op.
const MOCK: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, sys, time
root = pathlib.Path(__file__).resolve().parent
mode = (root / 'mode').read_text()
args = sys.argv[1:]
assert args[:2] in [['item', 'get'], ['item', 'edit']]
assert args[2:8] == ['item-a', '--vault', 'vault-a', '--format', 'json', '--reveal']
assert len(args) == 11 and args[8] == '--config' and args[10] == '--cache=false'
config = pathlib.Path(args[9])
assert config.is_absolute() and config.is_dir()
assert config.stat().st_mode & 0o777 == 0o700
assert config.stat().st_uid == os.getuid()
assert not list(config.iterdir())
with (root / 'config-paths').open('a') as log:
    log.write(str(config) + '\n')
(config / 'synthetic-cli-state').write_text('synthetic configuration metadata')
assert os.environ.get('OP_SERVICE_ACCOUNT_TOKEN') == 'synthetic-service-token'
assert {key for key in os.environ if key.startswith('OP_')} == {'OP_SERVICE_ACCOUNT_TOKEN'}
assert 'HOME' not in os.environ and 'XDG_CONFIG_HOME' not in os.environ
assert not any(key.lower().endswith('proxy') for key in os.environ)
with (root / 'calls').open('a') as log:
    log.write(args[1] + '\n')
if mode == 'timeout':
    (root / 'pid').write_text(str(os.getpid()))
    time.sleep(30)
if mode == 'error':
    print('synthetic-service-token synthetic-old-credential', file=sys.stderr)
    print('synthetic-new-credential')
    sys.exit(1)
if mode == 'oversized':
    sys.stdout.write('x' * (3 * 1024 * 1024))
    sys.exit(0)
if mode == 'stderr-oversized':
    sys.stderr.write('x' * (100 * 1024))
    sys.exit(0)
if mode == 'invalid-json':
    print('synthetic-old-credential NOT JSON')
    sys.exit(0)
item = json.loads((root / 'item.json').read_text())
if args[1] == 'edit':
    incoming = json.load(sys.stdin)
    assert not any(field.get('type') == 'DATE' and not field.get('value') for field in incoming['fields'])
    (root / 'submitted.json').write_text(json.dumps(incoming))
    item = incoming
    item['version'] += 1
    item['updated_at'] = 'synthetic-new-timestamp'
    item['last_edited_by'] = 'synthetic-service-editor'
    if mode == 'metadata-loss':
        item['title'] = 'changed-title'
    if mode == 'value-loss':
        item['fields'][0]['value'] = 'interfering-credential'
    if mode == 'version-interference':
        item['version'] += 1
    if mode == 'empty-date-normalization':
        item['fields'].append({'id':'empty-date','label':'Optional date','type':'DATE'})
        item['fields'][1].pop('value', None)
    (root / 'item.json').write_text(json.dumps(item))
    if mode == 'readback-interference':
        disturbed = json.loads(json.dumps(item))
        disturbed['version'] += 1
        disturbed['fields'][1]['value'] = 'external-edit'
        (root / 'item.json').write_text(json.dumps(disturbed))
print(json.dumps(item))
"#;

// exec keeps the recorded PID attached to the child the store must reap.
const SLEEPING_MOCK: &str = r#"#!/bin/sh
root=${0%/*}
while [ "$#" -gt 0 ]; do
    if [ "$1" = --config ]; then
        shift
        printf '%s\n' "$1" > "$root/config-paths"
    fi
    shift
done
printf '%s' "$$" > "$root/pid"
exec /bin/sleep 30
"#;

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
    store: OnePasswordStore,
}

fn mapping() -> VaultField {
    VaultField {
        credential: CredentialId::new("credential-a").unwrap(),
        vault: "vault-a".into(),
        item: "item-a".into(),
        field: "credential".into(),
    }
}
fn expected(generation: u64) -> CredentialRef {
    CredentialRef {
        id: CredentialId::new("credential-a").unwrap(),
        generation,
    }
}
fn item() -> Value {
    json!({
        "id":"item-a", "version":7, "vault":{"id":"vault-a","name":"Synthetic vault"},
        "title":"Synthetic credential", "category":"API_CREDENTIAL", "created_at":"synthetic-created",
        "updated_at":"synthetic-original-timestamp", "last_edited_by":"synthetic-enrolling-editor", "tags":["fixture"],
        "sections":[{"id":"metadata","label":"Metadata"}],
        "fields":[
            {"id":"credential","type":"CONCEALED","label":"Credential","value":"synthetic-old-credential","reference":"synthetic-reference"},
            {"id":"optional","type":"STRING","label":"Optional metadata","value":"","section":{"id":"metadata"}},
            {"id":"notes","type":"STRING","label":"Notes","value":"preserve-me","purpose":"NOTES"},
            {"id":"empty-date","type":"DATE","label":"Optional date","value":""},
            {"id":"real-date","type":"DATE","label":"Required date","value":"1700000000"}
        ]
    })
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap();
        let executable = path.join("mock-op");
        std::fs::write(&executable, MOCK).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(path.join("mode"), mode).unwrap();
        std::fs::write(path.join("item.json"), serde_json::to_vec(&item()).unwrap()).unwrap();
        let store = OnePasswordStore::new(
            vec![mapping()],
            SecretValue::new("synthetic-service-token".into()),
            executable,
        )
        .unwrap()
        .with_timeout(Duration::from_secs(2))
        .unwrap();
        Self {
            _directory: directory,
            path,
            store,
        }
    }
    fn calls(&self) -> String {
        std::fs::read_to_string(self.path.join("calls")).unwrap_or_default()
    }
    fn assert_configs_removed(&self, count: usize) {
        let paths = std::fs::read_to_string(self.path.join("config-paths")).unwrap();
        let paths: Vec<_> = paths.lines().collect();
        assert_eq!(paths.len(), count);
        assert_eq!(
            paths
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            count
        );
        for path in paths {
            assert!(!std::path::Path::new(path).exists());
        }
    }
    fn update(&self, item: &Value) {
        std::fs::write(
            self.path.join("item.json"),
            serde_json::to_vec(item).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn reads_exact_generation_and_rotates_through_stdin_with_full_readback() {
    let fixture = Fixture::new("normal");
    let (reference, secret) = fixture.store.latest(&expected(7).id).await.unwrap();
    assert_eq!(reference, expected(7));
    assert_eq!(secret.expose(), "synthetic-old-credential");
    let next = fixture
        .store
        .replace(
            &reference,
            SecretValue::new("synthetic-new-credential".into()),
        )
        .await
        .unwrap();
    assert_eq!(next, expected(8));
    assert_eq!(fixture.calls(), "get\nget\nedit\nget\n");
    assert_eq!(
        fixture.store.load(&next).await.unwrap().expose(),
        "synthetic-new-credential"
    );
    fixture.assert_configs_removed(5);
    let submitted: Value =
        serde_json::from_slice(&std::fs::read(fixture.path.join("submitted.json")).unwrap())
            .unwrap();
    assert_eq!(submitted["fields"][0]["id"], "credential");
    assert_eq!(submitted["fields"][0]["type"], "CONCEALED");
    assert_eq!(submitted["fields"][0]["reference"], "synthetic-reference");
    assert_eq!(submitted["fields"][2]["value"], "preserve-me");
    assert_eq!(submitted["title"], item()["title"]);
    assert_eq!(submitted["sections"], item()["sections"]);
    assert_eq!(submitted["tags"], item()["tags"]);
    assert!(
        submitted["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|field| field["id"] != "empty-date")
    );
}

#[tokio::test]
async fn stale_generations_and_observed_rollbacks_fail_without_editing() {
    let fixture = Fixture::new("normal");
    fixture.store.latest(&expected(7).id).await.unwrap();
    assert_eq!(
        fixture.store.load(&expected(6)).await.unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
    assert_eq!(
        fixture
            .store
            .replace(&expected(6), SecretValue::new("unused".into()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::CredentialUnavailable
    );
    let mut restored = item();
    restored["version"] = json!(6);
    fixture.update(&restored);
    assert_eq!(
        fixture
            .store
            .latest(&expected(7).id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::CredentialUnavailable
    );
    assert!(!fixture.calls().contains("edit"));
}

#[tokio::test]
async fn racing_replacements_serialize_and_only_one_uses_the_expected_generation() {
    let fixture = Fixture::new("normal");
    let reference = expected(7);
    let first = fixture
        .store
        .replace(&reference, SecretValue::new("first-value".into()));
    let second = fixture
        .store
        .replace(&reference, SecretValue::new("second-value".into()));
    let (first, second) = tokio::join!(first, second);
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        fixture
            .calls()
            .lines()
            .filter(|line| *line == "edit")
            .count(),
        1
    );
}

#[tokio::test]
async fn empty_date_and_missing_empty_value_normalization_preserves_other_fields() {
    let fixture = Fixture::new("empty-date-normalization");
    let next = fixture
        .store
        .replace(&expected(7), SecretValue::new("new-value".into()))
        .await
        .unwrap();
    assert_eq!(next.generation, 8);
    let readback: Value =
        serde_json::from_slice(&std::fs::read(fixture.path.join("item.json")).unwrap()).unwrap();
    assert!(
        readback["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["id"] == "real-date" && field["value"] == "1700000000")
    );
}

#[tokio::test]
async fn metadata_value_and_version_interference_fail_closed() {
    for mode in [
        "metadata-loss",
        "value-loss",
        "version-interference",
        "readback-interference",
    ] {
        let fixture = Fixture::new(mode);
        assert_eq!(
            fixture
                .store
                .replace(&expected(7), SecretValue::new("new-value".into()))
                .await
                .unwrap_err()
                .code,
            ErrorCode::CredentialUnavailable,
            "{mode}"
        );
    }
}

#[tokio::test]
async fn subprocess_failures_and_malformed_outputs_do_not_expose_secret_text() {
    for mode in ["error", "invalid-json", "oversized", "stderr-oversized"] {
        let fixture = Fixture::new(mode);
        let error = fixture.store.load(&expected(7)).await.unwrap_err();
        let displayed = format!("{error:?} {error}");
        for secret in [
            "synthetic-service-token",
            "synthetic-old-credential",
            "synthetic-new-credential",
        ] {
            assert!(!displayed.contains(secret));
        }
        assert_eq!(error.code, ErrorCode::CredentialUnavailable);
        fixture.assert_configs_removed(1);
    }
}

#[tokio::test]
async fn timed_out_subprocess_is_killed_and_reaped() {
    let mut fixture = Fixture::new("timeout");
    // Avoid making Python interpreter startup part of the timeout assertion.
    std::fs::write(fixture.path.join("mock-op"), SLEEPING_MOCK).unwrap();
    fixture.store = fixture.store.with_timeout(Duration::from_secs(3)).unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        fixture.store.load(&expected(7)).await.unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
    assert!(started.elapsed() < Duration::from_secs(8));
    fixture.assert_configs_removed(1);
    let pid = std::fs::read_to_string(fixture.path.join("pid")).unwrap();
    // kill -0 only inspects the synthetic process, after the store reaped it.
    let alive = std::process::Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!alive.success());
}

#[tokio::test]
async fn cancelled_read_kills_child_and_removes_private_config() {
    let fixture = Fixture::new("timeout");
    std::fs::write(fixture.path.join("mock-op"), SLEEPING_MOCK).unwrap();
    let reference = expected(7);
    let mut operation = Box::pin(fixture.store.load(&reference));
    tokio::select! {
        result = &mut operation => panic!("synthetic child finished before cancellation: {result:?}"),
        _ = async {
            while std::fs::read_to_string(fixture.path.join("pid"))
                .ok()
                .is_none_or(|pid| pid.trim().is_empty())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } => {}
    }
    drop(operation);
    fixture.assert_configs_removed(1);
    let pid = std::fs::read_to_string(fixture.path.join("pid")).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let alive = std::process::Command::new("/bin/kill")
                .args(["-0", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            if !alive.success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled synthetic child must be killed and reaped");
}

#[tokio::test]
async fn mismatched_item_identity_duplicate_fields_and_unsupported_content_are_rejected() {
    for mutation in ["item", "vault", "duplicate", "passkey", "files"] {
        let fixture = Fixture::new("normal");
        let mut changed = item();
        match mutation {
            "item" => changed["id"] = json!("other-item"),
            "vault" => changed["vault"]["id"] = json!("other-vault"),
            "duplicate" => {
                let field = changed["fields"][0].clone();
                changed["fields"].as_array_mut().unwrap().push(field);
            }
            "passkey" => changed["passkeys"] = json!([{"id":"synthetic-passkey"}]),
            "files" => changed["files"] = json!([{"id":"synthetic-file"}]),
            _ => unreachable!(),
        }
        fixture.update(&changed);
        assert!(
            fixture
                .store
                .replace(&expected(7), SecretValue::new("new-value".into()))
                .await
                .is_err()
        );
        assert!(!fixture.calls().contains("edit"));
    }
}

#[test]
fn mappings_require_unique_items_and_credentials_and_trusted_executable_paths() {
    let token = || SecretValue::new("synthetic-service-token".into());
    let path = || PathBuf::from("/synthetic/op");
    assert!(OnePasswordStore::new(vec![mapping(), mapping()], token(), path()).is_err());
    let mut duplicate_item = mapping();
    duplicate_item.credential = CredentialId::new("credential-b").unwrap();
    assert!(OnePasswordStore::new(vec![mapping(), duplicate_item], token(), path()).is_err());
    let mut invalid = mapping();
    invalid.item = "--assignment=secret".into();
    assert!(OnePasswordStore::new(vec![invalid], token(), path()).is_err());
    assert!(OnePasswordStore::new(vec![mapping()], token(), PathBuf::from("op")).is_err());
}
