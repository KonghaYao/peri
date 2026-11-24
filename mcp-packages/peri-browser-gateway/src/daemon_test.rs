use super::*;

fn state() -> GatewayState {
    GatewayState {
        pid: 4242,
        bind: "127.0.0.1:7391".to_owned(),
        log: "/tmp/gateway-7391.log".to_owned(),
        engine_port: Some(18971),
        engine_owned: true,
        started_at: 1_700_000_000,
    }
}

fn bind() -> SocketAddr {
    "127.0.0.1:7391".parse().expect("static bind")
}

fn state_on(engine_port: Option<u16>, engine_owned: bool) -> GatewayState {
    GatewayState {
        engine_port,
        engine_owned,
        ..state()
    }
}

#[test]
fn state_files_are_named_by_port_and_round_trip() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = state_path(dir.path(), 7391);
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("gateway-7391.json")
    );
    assert_eq!(read_state(&path).expect("missing state"), None);

    write_state(&path, &state()).expect("write state");
    assert_eq!(read_state(&path).expect("read state"), Some(state()));

    remove_state(&path).expect("remove state");
    assert_eq!(read_state(&path).expect("removed state"), None);
    remove_state(&path).expect("remove is idempotent");
}

#[test]
fn a_state_whose_pid_is_gone_or_reused_is_stale() {
    assert_eq!(start_decision(None, None), StartDecision::Start);
    assert_eq!(
        start_decision(Some(&state()), None),
        StartDecision::CleanStale
    );
    assert_eq!(
        start_decision(Some(&state()), Some("/usr/bin/kitesurf")),
        StartDecision::CleanStale,
        "a recycled pid must not block start"
    );
    assert_eq!(
        start_decision(Some(&state()), Some("/opt/homebrew/bin/peri-browser")),
        StartDecision::AlreadyRunning { pid: 4242 }
    );
}

#[test]
fn a_state_belongs_to_exactly_its_bind() {
    let state = state();
    assert!(state_matches_bind(
        &state,
        "127.0.0.1:7391".parse().unwrap()
    ));
    assert!(
        !state_matches_bind(&state, "[::1]:7391".parse().unwrap()),
        "a port-keyed state must not cross loopback addresses"
    );
    let ipv6 = GatewayState {
        bind: "[::1]:7391".to_owned(),
        ..state
    };
    assert!(state_matches_bind(&ipv6, "[::1]:7391".parse().unwrap()));
    let corrupt = GatewayState {
        bind: "not-an-address".to_owned(),
        ..ipv6
    };
    assert!(!state_matches_bind(&corrupt, "[::1]:7391".parse().unwrap()));
}

#[test]
fn stop_is_idempotent_without_state_or_with_a_dead_pid() {
    assert_eq!(stop_decision(None, bind(), None), StopDecision::NotRunning);
    assert_eq!(
        stop_decision(Some(&state()), bind(), None),
        StopDecision::NotRunning,
        "a dead pid is a stale state, not a failure"
    );
}

#[test]
fn stop_refuses_a_live_pid_that_is_not_the_gateway() {
    assert_eq!(
        stop_decision(Some(&state()), bind(), Some("/usr/bin/kitesurf")),
        StopDecision::Refuse {
            pid: 4242,
            comm: "/usr/bin/kitesurf".to_owned()
        }
    );
}

#[test]
fn stop_never_crosses_to_the_other_loopback_address() {
    let other: SocketAddr = "[::1]:7391".parse().expect("ipv6 bind");
    assert_eq!(
        stop_decision(
            Some(&state()),
            other,
            Some("/opt/homebrew/bin/peri-browser")
        ),
        StopDecision::OtherBind,
        "a live gateway on 127.0.0.1:7391 must survive stop on [::1]:7391"
    );
}

#[test]
fn stop_terminates_only_the_gateway_basename() {
    for comm in [
        "peri-browser",
        "/Users/mino/code/peri/target/debug/peri-browser",
    ] {
        assert_eq!(
            stop_decision(Some(&state()), bind(), Some(comm)),
            StopDecision::Terminate { pid: 4242 },
            "comm={comm}"
        );
    }
    assert!(is_gateway_comm("peri-browser"));
    assert!(is_gateway_comm("/opt/homebrew/bin/peri-browser"));
    assert!(!is_gateway_comm("peri-browser-helper"));
    assert!(!is_gateway_comm("kitesurf"));
    assert!(!is_gateway_comm(""));
}

#[test]
fn an_owned_engine_stops_only_without_another_reference() {
    assert_eq!(engine_stop_target(&state(), &[]), Some(18971));

    let shared = state_on(Some(18971), false);
    assert_eq!(
        engine_stop_target(&state(), &[shared]),
        None,
        "another gateway on the same engine port blocks the engine stop"
    );

    let other_port = state_on(Some(18972), false);
    assert_eq!(engine_stop_target(&state(), &[other_port]), Some(18971));

    let reused = state_on(Some(18971), false);
    assert_eq!(
        engine_stop_target(&reused, &[]),
        None,
        "a reused engine belongs to whoever started it"
    );

    let no_engine = state_on(None, false);
    assert_eq!(engine_stop_target(&no_engine, &[]), None);
}

#[test]
fn run_directories_are_owner_only() {
    let home = tempfile::tempdir().expect("temp dir");
    let dirs = Dirs::new(home.path());
    dirs.create().expect("create dirs");
    dirs.create().expect("create dirs again");
    for dir in [&dirs.run, &dirs.logs] {
        let mode = fs::metadata(dir)
            .expect("dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} must be owner-only", dir.display());
    }
    assert_eq!(
        dirs.run,
        home.path().join(".peri").join("browser").join("run")
    );
    assert_eq!(
        dirs.logs,
        home.path().join(".peri").join("browser").join("logs")
    );
}

#[test]
fn other_state_files_are_ignored_when_the_engine_port_differs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let current = state_path(dir.path(), 7391);
    write_state(&current, &state()).expect("write current");
    write_state(&state_path(dir.path(), 7392), &state_on(Some(18971), false))
        .expect("write sharer");
    write_state(&state_path(dir.path(), 7393), &state_on(Some(18972), true))
        .expect("write other engine");
    fs::write(dir.path().join("notes.txt"), "ignored").expect("write notes");

    let mut others = other_states(dir.path(), &current);
    others.sort_by_key(|other| other.bind.clone());
    assert_eq!(others.len(), 2);
    assert_eq!(engine_stop_target(&state(), &others), None);

    remove_state(&state_path(dir.path(), 7392)).expect("remove sharer");
    let others = other_states(dir.path(), &current);
    assert_eq!(engine_stop_target(&state(), &others), Some(18971));
}
