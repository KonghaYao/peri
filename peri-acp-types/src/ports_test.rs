use std::sync::Arc;

use super::*;

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

    async fn shutdown(&self) {
        self.shutdown_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
        fn as_any(&self) -> &dyn Any {
            self
        }
        async fn shutdown(&self) {}
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
    impl ToolSearchPort for WrongType {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    let wrong: Arc<dyn ToolSearchPort> = Arc::new(WrongType);
    let err = Arc::clone(&wrong)
        .downcast_arc::<MockToolSearch>()
        .unwrap_err();
    assert!(
        Arc::ptr_eq(&wrong, &err),
        "类型不符必须原样返回原 Arc（Err 分支不吞引用）"
    );
}
