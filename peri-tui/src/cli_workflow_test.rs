use std::ffi::OsString;

use super::argv_requests_workflow;

#[test]
fn workflow_is_detected_before_configuration() {
    assert!(argv_requests_workflow(&[
        OsString::from("peri"),
        OsString::from("workflow"),
        OsString::from("help"),
    ]));
    assert!(argv_requests_workflow(&[
        OsString::from("peri"),
        OsString::from("--model"),
        OsString::from("test"),
        OsString::from("workflow"),
        OsString::from("help"),
    ]));
    assert!(!argv_requests_workflow(&[
        OsString::from("peri"),
        OsString::from("--print=foo"),
        OsString::from("workflow"),
        OsString::from("help"),
    ]));
}

#[test]
fn unrelated_commands_are_not_routed_to_workflow() {
    assert!(!argv_requests_workflow(&[
        OsString::from("peri"),
        OsString::from("meta"),
    ]));
    assert!(!argv_requests_workflow(&[OsString::from("peri")]));
    assert!(!argv_requests_workflow(&[
        OsString::from("peri"),
        OsString::from("--model"),
        OsString::from("workflow"),
        OsString::from("acp"),
    ]));
    assert!(!argv_requests_workflow(&[
        OsString::from("peri"),
        OsString::from("--"),
        OsString::from("workflow"),
        OsString::from("help"),
    ]));
    assert!(!argv_requests_workflow(&[
        OsString::from("peri"),
        OsString::from("acp"),
        OsString::from("--cwd"),
        OsString::from("workflow"),
    ]));
}
