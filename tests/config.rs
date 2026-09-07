use poolparty::{
    config::{MemoryCredentials, StateDirectory},
    domain::*,
    ports::*,
};

#[tokio::test]
async fn compare_generation_allows_only_one_concurrent_refresh_commit() {
    let reference = CredentialRef {
        id: CredentialId::new("credential-a").unwrap(),
        generation: 7,
    };
    let store = MemoryCredentials::new(vec![(
        reference.clone(),
        SecretValue::new("fixture-old".into()),
    )])
    .unwrap();
    let (a, b) = tokio::join!(
        store.replace(&reference, SecretValue::new("fixture-next-a".into())),
        store.replace(&reference, SecretValue::new("fixture-next-b".into()))
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let next = a.or(b).unwrap();
    assert_eq!(next.generation, 8);
    assert!(store.load(&reference).await.is_err());
    assert!(store.load(&next).await.is_ok());
    assert_eq!(
        format!("{:?}", store.load(&next).await.unwrap()),
        "SecretValue([REDACTED])"
    );
}

#[test]
fn state_directory_has_one_owner_until_guard_drops() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().canonicalize().unwrap().join("state");
    let first = StateDirectory::acquire(&path).unwrap();
    assert_eq!(first.database, path.join("poolparty.sqlite3"));
    assert!(StateDirectory::acquire(&path).is_err());
    drop(first);
    assert!(StateDirectory::acquire(path).is_ok());
}

#[test]
fn identifiers_validate_deserialization_and_path_separators() {
    assert!(serde_json::from_str::<BindingId>("\"../account\"").is_err());
    assert!(serde_json::from_str::<BindingId>("\"\"").is_err());
    assert!(BindingId::new("a".repeat(129)).is_err());
    assert_eq!(
        serde_json::from_str::<BindingId>("\"binding-a\"")
            .unwrap()
            .as_str(),
        "binding-a"
    );
}
