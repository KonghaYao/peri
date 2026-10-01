use std::{
    io,
    sync::{Arc, Barrier},
    thread,
};

use crate::ConfigurationClient;

#[test]
fn compare_and_write_distinguishes_missing_from_empty_files() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.json");
    let empty = directory.path().join("empty.json");
    std::fs::write(&empty, "").unwrap();
    let client = ConfigurationClient::local().unwrap();

    assert!(client
        .write_text_if_unchanged(&missing, &None, "created")
        .unwrap());
    assert!(client
        .write_text_if_unchanged(&empty, &Some(String::new()), "filled")
        .unwrap());
    assert!(!client
        .write_text_if_unchanged(&empty, &None, "must not replace")
        .unwrap());
    assert_eq!(client.read_text(&missing).unwrap(), "created");
    assert_eq!(client.read_text(&empty).unwrap(), "filled");
}

#[test]
fn compare_and_write_conflict_preserves_the_current_content() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    std::fs::write(&path, "newer-secret-value").unwrap();
    let client = ConfigurationClient::local().unwrap();

    assert!(!client
        .write_text_if_unchanged(&path, &Some("stale".into()), "replacement")
        .unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "newer-secret-value"
    );
}

#[test]
fn concurrent_compare_and_write_has_a_single_winner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    std::fs::write(&path, "initial").unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let writers = ["writer-a", "writer-b"].map(|content| {
        let client = ConfigurationClient::local().unwrap();
        let path = path.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            client.write_text_if_unchanged(&path, &Some("initial".into()), content)
        })
    });
    barrier.wait();
    let outcomes = writers.map(|writer| writer.join().unwrap().unwrap());

    assert_eq!(outcomes.into_iter().filter(|written| *written).count(), 1);
    assert!(matches!(
        std::fs::read_to_string(path).unwrap().as_str(),
        "writer-a" | "writer-b"
    ));
}

#[test]
fn compare_and_write_request_round_trips_missing_and_empty_expectations() {
    use peri_acp_types::configuration::ConfigurationRequest;

    for expected in [None, Some(String::new())] {
        let request = ConfigurationRequest::WriteTextIfUnchanged {
            path: "settings.json".into(),
            expected: expected.clone(),
            content: "updated".into(),
        };
        let encoded = serde_json::to_value(request).unwrap();
        let decoded: ConfigurationRequest = serde_json::from_value(encoded).unwrap();
        assert!(matches!(
            decoded,
            ConfigurationRequest::WriteTextIfUnchanged { expected: actual, .. }
                if actual == expected
        ));
    }
}

#[test]
fn compare_and_write_io_errors_do_not_disclose_file_content() {
    let directory = tempfile::tempdir().unwrap();
    let invalid_text = directory.path().join("invalid-secret.json");
    std::fs::write(&invalid_text, [0xff, 0xfe]).unwrap();
    let unreadable = directory.path().join("secret-directory");
    std::fs::create_dir(&unreadable).unwrap();
    let client = ConfigurationClient::local().unwrap();

    let error = client.read_text(&invalid_text).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!error.to_string().contains("secret"));

    let error = client
        .write_text_if_unchanged(&unreadable, &None, "replacement")
        .unwrap_err();
    assert!(!error.to_string().contains("secret"));
}
