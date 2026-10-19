//! `initialize` 的 peri 能力收敛：客户端声明必须与后端实际支持一致。
//!
//! 只回显客户端声明会把「客户端想要」冒充成「端到端可用」。窗口协议依赖
//! Store 的 bounds/page/body 读取端口；未实现的后端（remote/emscripten）不得
//! 因声明而被协商成可用。

use peri_acp_types::PeriCaps;
use serde_json::Value;

use super::super::AcpServerConfig;
use super::session_lifecycle::initialize_peri_caps;

/// 客户端声明收敛到当前后端的实际能力。
pub(crate) fn effective_peri_caps(cfg: &AcpServerConfig, params: &Value) -> PeriCaps {
    apply_backend_caps(
        initialize_peri_caps(params),
        cfg.session_resources.supports_history_window(),
    )
}

/// 纯能力收敛规则：客户端声明不能把未实现的后端协商成可用。
fn apply_backend_caps(mut caps: PeriCaps, supports_history_window: bool) -> PeriCaps {
    if !supports_history_window {
        caps.history_window_v1 = false;
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared_window_caps() -> PeriCaps {
        PeriCaps {
            history_window_v1: true,
            history_pagination: true,
            ..PeriCaps::default()
        }
    }

    #[test]
    fn unsupported_backend_cannot_be_negotiated_into_window_protocol() {
        let caps = apply_backend_caps(declared_window_caps(), false);
        assert!(!caps.history_window_v1);
        assert!(
            caps.history_pagination,
            "收敛只影响窗口端口，不篡改其他声明"
        );
    }

    #[test]
    fn supported_backend_keeps_client_declaration() {
        let caps = apply_backend_caps(declared_window_caps(), true);
        assert!(caps.history_window_v1);
    }
}
