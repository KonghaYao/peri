use std::sync::Arc;

use super::*;

#[derive(Default)]
struct MockMcpTaskOwner {
    began: std::sync::atomic::AtomicBool,
    shutdown_calls: usize,
}

#[async_trait::async_trait]
impl McpTaskOwnerPort for MockMcpTaskOwner {
    fn begin_shutdown(&self) {
        self.began.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    async fn shutdown(&mut self) -> McpTaskShutdownReport {
        self.shutdown_calls += 1;
        McpTaskShutdownReport::Complete
    }
}

#[tokio::test]
async fn test_mcp_task_owner_port_preserves_unique_mutable_shutdown_owner() {
    let mut owner: Box<dyn McpTaskOwnerPort> = Box::new(MockMcpTaskOwner::default());
    owner.begin_shutdown();
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
}

// -- McpPoolPort -----------------------------------------------------------

/// 测试用 mock：记录 shutdown 调用次数。
#[derive(Debug)]
struct MockMcpPool {
    shutdown_calls: std::sync::atomic::AtomicUsize,
}

impl MockMcpPool {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            shutdown_calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl McpPoolPort for MockMcpPool {
    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn shutdown(&self) -> McpPoolShutdownReport {
        self.shutdown_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"mock": true})
    }
}

/// [回归测试] McpPoolPort::downcast_arc 必须还原具体实例
/// （2026-08-06-e2e-workflow-not-completing 遗留项）。
///
/// 历史 bug：downcast_arc 直接对 trait object 调 `type_id()`——trait 不
/// 继承 `Any`，方法经 `Any` blanket impl 解析，返回
/// `TypeId::of::<dyn McpPoolPort>()`（trait object 自身），恒不等于
/// `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退临时空池，注入的
/// 连接池与装配产物分离（MCP 工具/中间件不生效）。
#[test]
fn test_mcp_pool_port_downcast_restores_concrete() {
    let concrete = MockMcpPool::new();
    let port: Arc<dyn McpPoolPort> = Arc::clone(&concrete) as Arc<dyn McpPoolPort>;

    // SAFETY: mock 的 as_any 返回 self。
    let restored = match unsafe { Arc::clone(&port).downcast_arc::<MockMcpPool>() } {
        Ok(pool) => pool,
        Err(_) => panic!("downcast 必须还原具体类型 MockMcpPool"),
    };
    assert!(
        Arc::ptr_eq(&concrete, &restored),
        "还原实例必须是原 Arc（注入池与装配产物共享同一实例）"
    );

    // 类型不符时返回原 Arc（不丢失引用），且仍可正常调用端口方法。
    #[derive(Default)]
    struct WrongType;
    #[async_trait::async_trait]
    impl McpPoolPort for WrongType {
        fn as_any(&self) -> &dyn Any {
            self
        }
        async fn shutdown(&self) -> McpPoolShutdownReport {
            McpPoolShutdownReport::Complete {
                settled_services: 0,
                failed_services: 0,
            }
        }
        fn snapshot(&self) -> serde_json::Value {
            serde_json::json!({})
        }
    }

    let wrong: Arc<dyn McpPoolPort> = Arc::new(WrongType);
    // SAFETY: WrongType 的 as_any 返回 self（类型不符走 Err 分支）。
    let err = unsafe { Arc::clone(&wrong).downcast_arc::<MockMcpPool>() }.unwrap_err();
    assert!(
        Arc::ptr_eq(&wrong, &err),
        "类型不符必须原样返回原 Arc（Err 分支不吞引用）"
    );
}

/// 包装实现把 `as_any` 指向内字段时，`downcast_arc` 必须拒绝而非按外层指针 cast。
///
/// 报告中的前提不成立：`as_any` 自报的 TypeId 可以匹配，但数据地址与外层
/// trait object 不同（内字段偏移）。地址校验必须让这条路径返回 `Err`。
#[test]
fn test_mcp_pool_downcast_rejects_wrapper_returning_inner_any() {
    #[repr(C)]
    struct Wrapper {
        // 保证 inner 不在 offset 0（复现「地址相差」的前提）。
        _prefix: u64,
        inner: MockMcpPool,
    }

    #[async_trait::async_trait]
    impl McpPoolPort for Wrapper {
        fn as_any(&self) -> &dyn Any {
            // 故意违反契约：返回内字段的 Any（TypeId 与外层对象不同）。
            &self.inner
        }

        async fn shutdown(&self) -> McpPoolShutdownReport {
            self.inner.shutdown().await
        }

        fn snapshot(&self) -> serde_json::Value {
            self.inner.snapshot()
        }
    }

    let wrapper: Arc<dyn McpPoolPort> = Arc::new(Wrapper {
        _prefix: 0,
        inner: MockMcpPool {
            shutdown_calls: std::sync::atomic::AtomicUsize::new(0),
        },
    });

    // SAFETY: 本测试故意违反 as_any 契约以激活实现层的地址校验；校验命中时
    // 不会按外层指针重建 Arc。契约违规本身仍是调用方的错误，不能模仿。
    let err = unsafe { Arc::clone(&wrapper).downcast_arc::<MockMcpPool>() }.unwrap_err();
    assert!(
        Arc::ptr_eq(&wrapper, &err),
        "内字段 Any 必须被拒绝并原样返回原 Arc（不得按外层指针 cast）"
    );
}

// -- ToolSearchPort --------------------------------------------------------

/// 测试用 mock 实现。
#[derive(Debug)]
struct MockToolSearch;

impl ToolSearchPort for MockToolSearch {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// [回归测试] ToolSearchPort::downcast_arc 必须还原具体实例
/// （2026-08-06-e2e-workflow-not-completing 遗留项）。
///
/// 历史 bug 与 McpPoolPort 同构：直接对 trait object 调 `type_id()` 命中
/// `Any` blanket impl → downcast 恒失败 → 装配面回退默认空索引，注入的
/// 搜索索引与装配产物分离（工具搜索不生效）。
#[test]
fn test_tool_search_port_downcast_restores_concrete() {
    let concrete = Arc::new(MockToolSearch);
    let port: Arc<dyn ToolSearchPort> = Arc::clone(&concrete) as Arc<dyn ToolSearchPort>;

    // SAFETY: mock 的 as_any 返回 self。
    let restored = match unsafe { Arc::clone(&port).downcast_arc::<MockToolSearch>() } {
        Ok(index) => index,
        Err(_) => panic!("downcast 必须还原具体类型 MockToolSearch"),
    };
    assert!(
        Arc::ptr_eq(&concrete, &restored),
        "还原实例必须是原 Arc（注入索引与装配产物共享同一实例）"
    );

    #[derive(Default)]
    struct WrongType;
    impl ToolSearchPort for WrongType {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    let wrong: Arc<dyn ToolSearchPort> = Arc::new(WrongType);
    // SAFETY: WrongType 的 as_any 返回 self（类型不符走 Err 分支）。
    let err = unsafe { Arc::clone(&wrong).downcast_arc::<MockToolSearch>() }.unwrap_err();
    assert!(
        Arc::ptr_eq(&wrong, &err),
        "类型不符必须原样返回原 Arc（Err 分支不吞引用）"
    );
}
