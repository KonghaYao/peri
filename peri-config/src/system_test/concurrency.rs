// ─── 已发布快照读取与配置源 I/O 的锁分离 ───────────────────────────────────

use std::sync::mpsc;
use std::sync::{Barrier, Condvar};
use std::thread;
use std::time::{Duration, Instant};

/// 暂停点：分别覆盖重载读取配置源与更新写出目标文件两类阻塞 I/O。
#[derive(Clone, Copy, PartialEq, Eq)]
enum PausePoint {
    Collect,
    Write,
}

#[derive(Default)]
struct GateState {
    armed: Option<PausePoint>,
    entered: bool,
    released: bool,
}

#[derive(Default)]
struct PauseGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl PauseGate {
    fn arm(&self, point: PausePoint) {
        let mut state = self.state.lock().unwrap();
        *state = GateState {
            armed: Some(point),
            entered: false,
            released: false,
        };
    }

    fn enter(&self, point: PausePoint) {
        let mut state = self.state.lock().unwrap();
        if state.armed != Some(point) {
            return;
        }
        state.armed = None;
        state.entered = true;
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn wait_until_entered(&self) {
        let mut state = self.state.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !state.entered {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (next, timeout) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
            assert!(
                !timeout.timed_out() || state.entered,
                "配置源未在 5s 内进入暂停点"
            );
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.armed = None;
        state.released = true;
        self.changed.notify_all();
    }
}

struct PausingSource {
    inner: FakeSource,
    gate: PauseGate,
}

impl PausingSource {
    fn new() -> Self {
        Self {
            inner: FakeSource::default(),
            gate: PauseGate::default(),
        }
    }
}

impl ConfigurationSource for PausingSource {
    fn collect(&self, scope: &ConfigurationScope) -> io::Result<ConfigurationInputs> {
        self.gate.enter(PausePoint::Collect);
        self.inner.collect(scope)
    }

    fn write_if_unchanged(
        &self,
        path: &Path,
        expected: Option<&str>,
        content: &str,
    ) -> io::Result<bool> {
        self.gate.enter(PausePoint::Write);
        self.inner.write_if_unchanged(path, expected, content)
    }
}

fn make_pausing_system(
    scope: &ConfigurationScope,
    inputs: ConfigurationInputs,
) -> (Arc<PausingSource>, ConfigurationSystem) {
    let source = Arc::new(PausingSource::new());
    source.inner.set_inputs(scope, inputs);
    let system = ConfigurationSystem::new(source.clone());
    (source, system)
}

/// 在独立线程读取 `current`，超时即证明读路径被配置源 I/O 阻塞。
fn read_current_within(
    system: &Arc<ConfigurationSystem>,
    scope: &ConfigurationScope,
    limit: Duration,
) -> Result<Option<Arc<ConfigurationSnapshot>>, mpsc::RecvTimeoutError> {
    let system = Arc::clone(system);
    let scope = scope.clone();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(system.current(&scope));
    });
    receiver.recv_timeout(limit)
}

#[test]
fn current_reads_published_snapshot_while_reload_waits_for_source() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_pausing_system(
        &scope,
        make_inputs(Some(r#"{"config":{"active_alias":"sonnet"}}"#), None, None),
    );
    let before = system.resolve(scope.clone()).unwrap();
    source.inner.mutate_inputs(&scope, |inputs| {
        inputs.global = Some(r#"{"config":{"active_alias":"haiku"}}"#.into());
    });
    let system = Arc::new(system);

    source.gate.arm(PausePoint::Collect);
    let writer = {
        let system = Arc::clone(&system);
        let scope = scope.clone();
        thread::spawn(move || system.resolve(scope))
    };
    source.gate.wait_until_entered();

    let observed = read_current_within(&system, &scope, Duration::from_secs(2));
    source.gate.release();
    let observed = observed
        .expect("已发布快照的读取不得等待配置源")
        .expect("旧快照仍处于发布状态");
    assert!(Arc::ptr_eq(&observed, &before));
    assert_eq!(observed.settings().config.active_alias, "sonnet");

    let reloaded = writer.join().unwrap().unwrap();
    assert_eq!(reloaded.settings().config.active_alias, "haiku");
}

#[test]
fn current_reads_published_snapshot_while_update_waits_for_write() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_pausing_system(&scope, make_inputs(None, None, None));
    let before = system.resolve(scope.clone()).unwrap();
    let mut candidate = before.settings().clone();
    candidate.config.language = Some("en".into());
    let system = Arc::new(system);

    source.gate.arm(PausePoint::Write);
    let writer = {
        let system = Arc::clone(&system);
        let scope = scope.clone();
        let revision = before.revision();
        thread::spawn(move || system.update(&scope, revision, &candidate))
    };
    source.gate.wait_until_entered();

    let observed = read_current_within(&system, &scope, Duration::from_secs(2));
    source.gate.release();
    let observed = observed
        .expect("已发布快照的读取不得等待配置写入")
        .expect("旧快照仍处于发布状态");
    assert!(Arc::ptr_eq(&observed, &before));
    assert_eq!(observed.settings().config.language, None);

    let updated = writer.join().unwrap().unwrap();
    assert_eq!(updated.settings().config.language.as_deref(), Some("en"));
    assert_ne!(updated.revision(), before.revision());
}

#[test]
fn concurrent_updates_accept_one_revision_and_reject_stale_writer() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(None, None, None));
    let before = system.resolve(scope.clone()).unwrap();
    let system = Arc::new(system);
    let barrier = Arc::new(Barrier::new(3));
    let workers: Vec<_> = ["en", "fr"]
        .into_iter()
        .map(|language| {
            let system = Arc::clone(&system);
            let barrier = Arc::clone(&barrier);
            let scope = scope.clone();
            let revision = before.revision();
            let mut candidate = before.settings().clone();
            candidate.config.language = Some(language.into());
            thread::spawn(move || {
                barrier.wait();
                system.update(&scope, revision, &candidate)
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        outcomes.iter().filter(|result| result.is_ok()).count(),
        1,
        "同一 revision 的并发写只能有一个成功"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(ConfigurationError::Conflict)))
            .count(),
        1,
        "落败的写必须报告 revision 冲突"
    );
    assert_eq!(source.writes().len(), 1);
}
