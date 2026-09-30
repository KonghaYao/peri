use super::load_or_create;

#[test]
fn identity_is_stable_and_a_uuid() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("machine-id");
    let identity = load_or_create(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), identity);
    assert_eq!(identity, load_or_create(&path).unwrap());
    assert_eq!(
        uuid::Uuid::parse_str(&identity).unwrap().get_version_num(),
        4
    );
}

#[test]
fn concurrent_initialization_publishes_one_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("machine-id");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                load_or_create(&path).unwrap()
            })
        })
        .collect();
    let identities: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(identities.iter().all(|identity| identity == &identities[0]));
    assert_eq!(load_or_create(&path).unwrap(), identities[0]);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn corrupt_identity_is_not_silently_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("machine-id");
    std::fs::write(&path, "invalid").unwrap();
    assert!(load_or_create(&path).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "invalid");
}
