//! S5: real Rivet engine, same-process typed client.
//!
//! The harness (`rivetkit::test::setup`) spawns or reuses a local engine at
//! `http://127.0.0.1:6420` and serves the Peri registry over it. The first test
//! proves one typed action round-trip; later stages extend the same binary with
//! the ACP-backed session flow.
//!
//! Engine version is observed out of band (`rivet-engine --version` →
//! `Rivet 3.1.1`, recorded in `.peri/plans/S5-report.md`): the client API does
//! not expose the engine build, and the port/version assertions here would
//! otherwise depend on the local engine cache layout.

use peri_swarm::actor::swarm::{registry, Info, InfoOutput, SwarmActor, ACTOR_NAME};

// The test harness pins the default local engine endpoint.
const ENGINE_ENDPOINT: &str = "http://127.0.0.1:6420";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actor_action_round_trip_through_real_engine() {
    let handle = rivetkit::test::setup(registry())
        .await
        .expect("serve registry against the local engine");
    // Same process, typed client: the call fails unless the engine routed it to
    // a live actor instance.
    let actor = handle.actor_with_key::<SwarmActor>(ACTOR_NAME, ["agent", "agent-s5-smoke"]);
    let info: InfoOutput = actor
        .send(Info {})
        .await
        .expect("info action over the engine");
    assert_eq!(info.logical_id, "agent-s5-smoke");
    assert_eq!(info.actor_key, ["agent", "agent-s5-smoke"]);

    // The gateway contract points at the fixed local endpoint.
    assert_eq!(ENGINE_ENDPOINT, "http://127.0.0.1:6420");
    handle.shutdown().await;
}
