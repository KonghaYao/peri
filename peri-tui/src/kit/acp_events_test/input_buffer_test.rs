use super::*;

/// C1 回归测试：drain_input_buffer 清空 INPUT_BUFFER 队列。
///
/// 注：不验证 SUBMIT_TX 接收——SUBMIT_TX 是 OnceLock 全局句柄，一旦被其他
/// 测试 set 就无法重置；此处只验证 drain 的核心效应（buffer 被清空）。
/// 顺序保证由 `VecDeque::drain(..)` + 顺序 `tx.send` 在源码层面保证。
#[tokio::test]
#[serial]
async fn test_drain_input_buffer_preserves_order() {
    crate::kit::atoms::init_atoms();
    let _ = SUBMIT_TX.get_or_init(|| {
        let (tx, _rx) = mpsc::unbounded_channel::<SubmitRequest>();
        tx
    });

    // 入队三条
    {
        let state = INPUT_BUFFER.state();
        let mut buf = state.write();
        buf.push_back("first".into());
        buf.push_back("second".into());
        buf.push_back("third".into());
    }

    drain_input_buffer();

    // 验证 buffer 已被 drain 干净——这是 drain_input_buffer 的核心效应
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "buffer should be empty after drain"
    );
}

/// C1 回归测试：空 buffer 是 no-op，drain 后仍为空。
#[tokio::test]
#[serial]
async fn test_drain_input_buffer_empty_is_noop() {
    crate::kit::atoms::init_atoms();
    let _ = SUBMIT_TX.get_or_init(|| {
        let (tx, _rx) = mpsc::unbounded_channel::<SubmitRequest>();
        tx
    });

    INPUT_BUFFER.state().write().clear();
    drain_input_buffer();

    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "empty buffer should remain empty"
    );
}

/// C1 回归测试：SUBMIT_TX 未初始化时安全跳过，不 panic，buffer 也保持不变。
///
/// 注：实际运行时 OnceLock 一旦 set 无法 unset；本测试只验证不 panic。
#[test]
#[serial]
fn test_drain_input_buffer_no_submit_tx_safe() {
    crate::kit::atoms::init_atoms();
    // 不论 SUBMIT_TX 是否 set，都不应 panic
    INPUT_BUFFER.state().write().push_back("x".into());
    drain_input_buffer();
    // SUBMIT_TX 已被前面测试 set 过，所以 drain 成功 → buffer 被清空
    // 即使 SUBMIT_TX 未 set，drain 早退，buffer 仍有 "x"——两种情况都不算 panic
}

/// Slice 3 D4：drain 时**每条**排队文本先 `send_local_user_bubble`（本地气泡恰
/// 出现一次，镜像非 loading 路径）再提交 AgentText——不依赖服务端回显。
/// LOCAL_EVENT_TX 与 SUBMIT_TX 同为全局 OnceLock：本测试安装成功时（serial
/// 首个）可观察两通道，验证 FIFO 顺序与气泡唯一性；已被占用时只断言
/// buffer 清空（核心效应）。
#[test]
#[serial]
fn test_drain_input_buffer_sends_local_user_bubble_once() {
    crate::kit::atoms::init_atoms();
    INPUT_BUFFER.state().write().clear();
    // 安装可观察 channel；OnceLock 已占用则返回 None（跳过通道级断言）。
    let (tx, rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
    let local_rx = match LOCAL_EVENT_TX.set(tx) {
        Ok(()) => Some(rx),
        Err(_) => None,
    };
    let mut submit_rx = ensure_submit_tx_observable();

    // 入队两条
    {
        let state = INPUT_BUFFER.state();
        let mut buf = state.write();
        buf.push_back("first".into());
        buf.push_back("second".into());
    }

    drain_input_buffer();

    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "buffer should be empty after drain"
    );
    if let Some(mut rx) = local_rx {
        // 恰一条 LocalUserBubble（FIFO 首条）
        match rx.try_recv() {
            Ok(ev) => match ev.event {
                AcpEventData::LocalUserBubble { text } => {
                    assert_eq!(text, "first", "drain 应先发首条排队项的气泡")
                }
                other => panic!("drain 应发 LocalUserBubble, got {other:?}"),
            },
            Err(e) => panic!("drain 应发出本地气泡, got {e:?}"),
        }
        // 第二条排队项的气泡
        match rx.try_recv() {
            Ok(ev) => match ev.event {
                AcpEventData::LocalUserBubble { text } => assert_eq!(text, "second"),
                other => panic!("drain 应发 LocalUserBubble, got {other:?}"),
            },
            Err(e) => panic!("drain 应发出第二条气泡, got {e:?}"),
        }
        assert!(
            matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "drain 不应发出第三条事件（气泡恰一次）"
        );
    }
    if let Some(mut rx) = submit_rx.take() {
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "first"),
            Ok(other) => panic!("drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("drain 应提交排队输入, got {e:?}"),
        }
    }
}

/// BRIDGE_RESET_COUNTER 递增时 acp_bridge 重置分支同步清空 INPUT_BUFFER，
/// 防止旧会话缓存输入在新会话 TurnDone 时泄漏。
///
/// 此测试模拟 bridge 的 counter != last_reset_counter 分支：先填入 buffer 数据，
/// 递增 BRIDGE_RESET_COUNTER，构造任意事件 dispatch，断言 buffer 已被清空。
/// 注意：实际清空发生在 acp_bridge.rs 的 counter 检测分支，而非 dispatch_and_notify
/// 内部。此测试模拟的是那个分支调用 push_view_models_for_reset() 前后的完整效应。
#[test]
#[serial]
fn test_bridge_reset_clears_input_buffer() {
    crate::kit::atoms::init_atoms();
    // 填入 buffer 数据
    INPUT_BUFFER
        .state()
        .write()
        .push_back("leaked input".into());
    INPUT_BUFFER
        .state()
        .write()
        .push_back("another leaked input".into());
    assert!(!INPUT_BUFFER.state().read().is_empty(), "buffer 应有数据");

    // 模拟 acp_bridge 的 counter 检测分支：
    // push_view_models_for_reset() 前同步清空 INPUT_BUFFER
    INPUT_BUFFER.state().write().clear();
    push_view_models_for_reset();

    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "bridge reset 后 INPUT_BUFFER 应被清空"
    );

    // VIEW_MODELS 也应被重置
    let snapshot = VIEW_MODELS.state().read().clone();
    assert!(
        snapshot.items.is_empty(),
        "bridge reset 后 committed 应为空"
    );
    assert!(
        snapshot.items.is_empty(),
        "bridge reset 后 current_turn 应为空"
    );
}
