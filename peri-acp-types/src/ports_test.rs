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

    let restored = match Arc::clone(&port).downcast_arc::<MockMcpPool>() {
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
    let err = Arc::clone(&wrong)
        .downcast_arc::<MockMcpPool>()
        .unwrap_err();
    assert!(
        Arc::ptr_eq(&wrong, &err),
        "类型不符必须原样返回原 Arc（Err 分支不吞引用）"
    );
}

/// 包装实现即使把内字段放在 offset 0（与 trait object 数据地址相同），
/// `downcast_arc` 也只能还原为包装类型自身，不能按地址猜成内字段类型。
#[test]
fn test_mcp_pool_downcast_rejects_offset_zero_wrapper() {
    #[repr(C)]
    struct OffsetZeroWrapper {
        inner: MockMcpPool,
        tail: [u8; 1024],
    }

    #[async_trait::async_trait]
    impl McpPoolPort for OffsetZeroWrapper {
        async fn shutdown(&self) -> McpPoolShutdownReport {
            self.inner.shutdown().await
        }

        fn snapshot(&self) -> serde_json::Value {
            self.inner.snapshot()
        }
    }

    let wrapper = Arc::new(OffsetZeroWrapper {
        inner: MockMcpPool {
            shutdown_calls: std::sync::atomic::AtomicUsize::new(0),
        },
        tail: [0; 1024],
    });
    assert!(
        std::mem::size_of::<OffsetZeroWrapper>() > std::mem::size_of::<MockMcpPool>(),
        "布局必须不同，才能覆盖「外层分配按内字段类型重建」的风险"
    );
    let port: Arc<dyn McpPoolPort> = Arc::clone(&wrapper) as Arc<dyn McpPoolPort>;

    let err = Arc::clone(&port)
        .downcast_arc::<MockMcpPool>()
        .err()
        .unwrap_or_else(|| panic!("offset 0 包装器不得被还原为内字段类型"));
    assert!(Arc::ptr_eq(&port, &err), "失败必须原样返回原 Arc");

    let restored = port
        .downcast_arc::<OffsetZeroWrapper>()
        .unwrap_or_else(|_| panic!("包装器应还原为自身类型"));
    assert!(Arc::ptr_eq(&wrapper, &restored));
}

/// 非零 offset 的包装实现同样只能还原为自身类型。
#[test]
fn test_mcp_pool_downcast_rejects_nonzero_offset_wrapper() {
    #[repr(C)]
    struct Wrapper {
        // 保证 inner 不在 offset 0，且旧实现的数据指针校验会拒绝该形状。
        _prefix: u64,
        inner: MockMcpPool,
    }

    #[async_trait::async_trait]
    impl McpPoolPort for Wrapper {
        async fn shutdown(&self) -> McpPoolShutdownReport {
            self.inner.shutdown().await
        }

        fn snapshot(&self) -> serde_json::Value {
            self.inner.snapshot()
        }
    }

    let wrapper = Arc::new(Wrapper {
        _prefix: 0,
        inner: MockMcpPool {
            shutdown_calls: std::sync::atomic::AtomicUsize::new(0),
        },
    });
    let port: Arc<dyn McpPoolPort> = Arc::clone(&wrapper) as Arc<dyn McpPoolPort>;

    let err = Arc::clone(&port)
        .downcast_arc::<MockMcpPool>()
        .err()
        .unwrap_or_else(|| panic!("内字段类型不得从包装器中还原"));
    assert!(
        Arc::ptr_eq(&port, &err),
        "类型不符必须原样返回原 Arc（不得按外层指针 cast）"
    );

    let restored = port
        .downcast_arc::<Wrapper>()
        .unwrap_or_else(|_| panic!("包装器应还原为自身类型"));
    assert!(Arc::ptr_eq(&wrapper, &restored));
}

// -- ToolSearchPort --------------------------------------------------------

/// 测试用 mock 实现。
#[derive(Debug)]
struct MockToolSearch;

impl ToolSearchPort for MockToolSearch {}

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

    let restored = match Arc::clone(&port).downcast_arc::<MockToolSearch>() {
        Ok(index) => index,
        Err(_) => panic!("downcast 必须还原具体类型 MockToolSearch"),
    };
    assert!(
        Arc::ptr_eq(&concrete, &restored),
        "还原实例必须是原 Arc（注入索引与装配产物共享同一实例）"
    );

    #[derive(Default)]
    struct WrongType;
    impl ToolSearchPort for WrongType {}

    let wrong: Arc<dyn ToolSearchPort> = Arc::new(WrongType);
    let err = Arc::clone(&wrong)
        .downcast_arc::<MockToolSearch>()
        .unwrap_err();
    assert!(
        Arc::ptr_eq(&wrong, &err),
        "类型不符必须原样返回原 Arc（Err 分支不吞引用）"
    );
}

/// [回归测试] offset 0 包装器（报告中的 UB 反例）：内字段与外层 trait object
/// 数据地址相同，`downcast_arc` 仍必须以对象自身类型为准拒绝内字段类型。
#[test]
fn test_tool_search_port_offset_zero_wrapper_is_not_inner_type() {
    #[repr(C)]
    struct OffsetZeroWrapper {
        inner: MockToolSearch,
        tail: [u8; 1024],
    }

    impl ToolSearchPort for OffsetZeroWrapper {}

    let wrapper = Arc::new(OffsetZeroWrapper {
        inner: MockToolSearch,
        tail: [0; 1024],
    });
    assert!(
        std::mem::size_of::<OffsetZeroWrapper>() > std::mem::size_of::<MockToolSearch>(),
        "布局必须不同，才能覆盖「外层分配按内字段类型重建」的风险"
    );
    let port: Arc<dyn ToolSearchPort> = Arc::clone(&wrapper) as Arc<dyn ToolSearchPort>;

    let err = Arc::clone(&port)
        .downcast_arc::<MockToolSearch>()
        .err()
        .unwrap_or_else(|| panic!("offset 0 包装器不得被还原为内字段类型"));
    assert!(Arc::ptr_eq(&port, &err), "失败必须原样返回原 Arc");

    let restored = port
        .downcast_arc::<OffsetZeroWrapper>()
        .unwrap_or_else(|_| panic!("包装器应还原为自身类型"));
    assert!(Arc::ptr_eq(&wrapper, &restored));
}

/// 非零 offset 包装器同样只能还原为自身类型。
#[test]
fn test_tool_search_port_nonzero_offset_wrapper_is_not_inner_type() {
    #[repr(C)]
    struct OffsetWrapper {
        prefix: u64,
        inner: MockToolSearch,
    }

    impl ToolSearchPort for OffsetWrapper {}

    let wrapper = Arc::new(OffsetWrapper {
        prefix: 0,
        inner: MockToolSearch,
    });
    let port: Arc<dyn ToolSearchPort> = Arc::clone(&wrapper) as Arc<dyn ToolSearchPort>;

    let err = Arc::clone(&port)
        .downcast_arc::<MockToolSearch>()
        .err()
        .unwrap_or_else(|| panic!("内字段类型不得从包装器中还原"));
    assert!(Arc::ptr_eq(&port, &err));

    let restored = port
        .downcast_arc::<OffsetWrapper>()
        .unwrap_or_else(|_| panic!("包装器应还原为自身类型"));
    assert!(Arc::ptr_eq(&wrapper, &restored));
}

// -- AgentCatalogPort ------------------------------------------------------

struct MockCatalog;

impl AgentCatalogPort for MockCatalog {
    fn catalog(&self, _include_builtin: bool) -> Vec<AgentCatalogEntry> {
        Vec::new()
    }
}

#[test]
fn test_agent_catalog_port_downcast_restores_concrete() {
    let concrete = Arc::new(MockCatalog);
    let port: Arc<dyn AgentCatalogPort> = Arc::clone(&concrete) as Arc<dyn AgentCatalogPort>;

    let restored = Arc::clone(&port)
        .downcast_arc::<MockCatalog>()
        .unwrap_or_else(|_| panic!("downcast 必须还原具体类型 MockCatalog"));
    assert!(Arc::ptr_eq(&concrete, &restored));

    let wrong: Arc<dyn AgentCatalogPort> = Arc::new(MockCatalog);
    let err = Arc::clone(&wrong)
        .downcast_arc::<OtherCatalog>()
        .err()
        .unwrap_or_else(|| panic!("类型不符必须失败"));
    assert!(Arc::ptr_eq(&wrong, &err));
}

struct OtherCatalog;

impl AgentCatalogPort for OtherCatalog {
    fn catalog(&self, _include_builtin: bool) -> Vec<AgentCatalogEntry> {
        Vec::new()
    }
}

/// offset 0 包装器只能还原为自身类型（目录端口同构反例）。
#[test]
fn test_agent_catalog_port_offset_zero_wrapper_is_not_inner_type() {
    #[repr(C)]
    struct OffsetZeroWrapper {
        inner: MockCatalog,
        tail: [u8; 1024],
    }

    impl AgentCatalogPort for OffsetZeroWrapper {
        fn catalog(&self, include_builtin: bool) -> Vec<AgentCatalogEntry> {
            self.inner.catalog(include_builtin)
        }
    }

    let wrapper = Arc::new(OffsetZeroWrapper {
        inner: MockCatalog,
        tail: [0; 1024],
    });
    let port: Arc<dyn AgentCatalogPort> = Arc::clone(&wrapper) as Arc<dyn AgentCatalogPort>;

    let err = Arc::clone(&port)
        .downcast_arc::<MockCatalog>()
        .err()
        .unwrap_or_else(|| panic!("offset 0 包装器不得被还原为内字段类型"));
    assert!(Arc::ptr_eq(&port, &err));

    let restored = port
        .downcast_arc::<OffsetZeroWrapper>()
        .unwrap_or_else(|_| panic!("包装器应还原为自身类型"));
    assert!(Arc::ptr_eq(&wrapper, &restored));
}
