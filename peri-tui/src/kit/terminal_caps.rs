//! 终端能力探测与符号降级（规格 §4.1 / §12）。
//!
//! - [`detect_caps`]：启动时探测一次（NO_COLOR / TERM=dumb / COLORTERM / TERM_PROGRAM /
//!   PERI_IMAGE / TMUX），写入 `atoms::TERMINAL_CAPS`，进程生命周期内只读。
//! - [`symbols`]：按 unicode 能力选择 §4.1 的 Unicode 符号集或 ASCII 后备符号集。
//!
//! 不变式：任何状态都不能只依赖颜色（§12）——符号集降级表与规格 §4.1 逐条对应。

/// 终端图片协议能力位。ITerm2 保持「检测但 disabled」语义（§6.3）：
/// 能力位记录存在，但本期任何渲染层都把 ITerm2 视同 None（未验证不启用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphicsProtocol {
    /// kitty graphics protocol（Kitty / Ghostty / WezTerm / Warp 品牌映射）。
    Kitty,
    /// iTerm2 inline image（OSC 1337）。能力位保留，渲染层当前视同 [`None`]。
    ITerm2,
    /// 无图片协议——像素渲染不可用，恒降级文本。
    #[default]
    None,
}

/// 终端能力集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCaps {
    /// 颜色输出是否可用（NO_COLOR 存在或 TERM=dumb 时禁用）。
    pub color: bool,
    /// Unicode 符号是否可用（TERM=dumb / LANG=C 时降级 ASCII）。
    pub unicode: bool,
    /// italic 是否可靠（Apple Terminal 不支持 italic，降级 dim）。
    pub italic: bool,
    /// 24-bit truecolor 是否可用（无 truecolor 时映射 ANSI 近似值）。
    pub truecolor: bool,
    /// 图片协议能力（P0/P1 门控位；§6.1 Q5 宁可 false negative 不可 false positive）。
    /// Default = None：未探测（单测/未知环境）时无像素渲染，安全默认。
    pub graphics: GraphicsProtocol,
}

impl Default for TerminalCaps {
    fn default() -> Self {
        // 默认全能力——未探测（如单测环境）时不做任何剥离/降级，保持现状行为。
        // graphics 例外：默认 None 是刻意的安全默认，防误判污染终端（§6.1 Q5）。
        Self {
            color: true,
            unicode: true,
            italic: true,
            truecolor: true,
            graphics: GraphicsProtocol::None,
        }
    }
}

/// 符号集——§4.1 表格的每个语义一个符号，外加按同一降级契约（§12）收编的
/// 辅助字形（focus border / reasoning 续行 / todo change 图标）——Unicode
/// 能力不足时统一降级 ASCII，不绕过降级表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolSet {
    /// running
    pub running: &'static str,
    /// success
    pub success: &'static str,
    /// error
    pub error: &'static str,
    /// warning / approval
    pub warning: &'static str,
    /// collapsed
    pub collapsed: &'static str,
    /// expanded
    pub expanded: &'static str,
    /// queued
    pub queued: &'static str,
    /// user prompt
    pub user_prompt: &'static str,
    /// todo change：Started（进行中）
    pub todo_started: &'static str,
    /// todo change：Reopened（重新打开）
    pub todo_reopened: &'static str,
    /// todo change：ActiveFormUpdated（表单更新）
    pub todo_edited: &'static str,
}

const UNICODE_SYMBOLS: SymbolSet = SymbolSet {
    running: "\u{25d0}", // ◐
    success: "\u{2713}", // ✓
    error: "\u{d7}",     // ×
    warning: "!",
    collapsed: "\u{25b8}",     // ▸
    expanded: "\u{25be}",      // ▾
    queued: "\u{b7}",          // ·
    user_prompt: "\u{203a}",   // ›
    todo_started: "\u{25b6}",  // ▶
    todo_reopened: "\u{21bb}", // ↻
    todo_edited: "\u{270e}",   // ✎
};

const ASCII_SYMBOLS: SymbolSet = SymbolSet {
    running: "*",
    success: "+",
    error: "x",
    warning: "!",
    collapsed: ">",
    expanded: "v",
    queued: ".",
    user_prompt: ">",
    todo_started: ">",
    todo_reopened: "~",
    todo_edited: "*",
};

/// 按终端能力选择符号集：unicode 不可用时降级 ASCII（§4.1 降级表）。
pub fn symbols(caps: &TerminalCaps) -> SymbolSet {
    if caps.unicode {
        UNICODE_SYMBOLS
    } else {
        ASCII_SYMBOLS
    }
}

/// 从进程环境探测终端能力。仅在启动时调用一次（entry.rs）。
pub fn detect_caps() -> TerminalCaps {
    detect_caps_from(&|name| std::env::var(name).ok())
}

/// 图片协议探测：品牌映射 + 环境变量 override（§6.3 E5 / image-p0-p1-spec §1.4）。
/// 仅启动时调用一次（entry.rs）。
/// 优先级：`PERI_IMAGE` override > 品牌映射 > None。
pub fn detect_graphics_protocol_from(get: &dyn Fn(&str) -> Option<String>) -> GraphicsProtocol {
    // PERI_IMAGE override（最高优先级，值解析为小写）：off=禁用 / kitty=强制 / iterm2=强制。
    // 空值视为未设置；未知值回落安全默认（宁可 false negative 不可 false positive）。
    if let Some(value) = get("PERI_IMAGE")
        && !value.is_empty()
    {
        return match value.to_ascii_lowercase().as_str() {
            "kitty" => GraphicsProtocol::Kitty,
            "iterm2" => GraphicsProtocol::ITerm2,
            // "off" 及任何未知值 → None
            _ => GraphicsProtocol::None,
        };
    }

    // TMUX 存在 → None：tmux 内嵌会转发/剥协议序列，即使 TERM_PROGRAM 残留外层
    // 终端品牌也禁用（矩阵硬性规则，宁可 false negative）。
    if get("TMUX").is_some() {
        return GraphicsProtocol::None;
    }

    // Windows ConPTY 会剥 APC（kitty protocol 依赖 APC），恒 None。
    // 作为尾表达式（下方品牌映射仅非 Windows 编译，Windows 下此即函数尾）。
    #[cfg(target_os = "windows")]
    {
        GraphicsProtocol::None
    }

    // 品牌映射（大小写不敏感小写比较）；tmux / 未知 / 未设置 → None（安全默认）。
    // 仅非 Windows 编译（Windows 已在上方恒 None 早退，避免 unreachable code）。
    #[cfg(not(target_os = "windows"))]
    return match get("TERM_PROGRAM")
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("kitty" | "ghostty" | "wezterm" | "warp") => GraphicsProtocol::Kitty,
        Some("iterm.app") => GraphicsProtocol::ITerm2,
        _ => GraphicsProtocol::None,
    };
}

/// 可注入环境读取器的探测实现（便于单测，不触碰真实 env）。
fn detect_caps_from(get: &dyn Fn(&str) -> Option<String>) -> TerminalCaps {
    // NO_COLOR 规范：变量存在即禁用颜色（值可为空）。
    let no_color = get("NO_COLOR").is_some();
    let term = get("TERM").unwrap_or_default();
    let dumb = term == "dumb";

    // Unicode：dumb 终端或 C/POSIX locale（无 UTF-8 后缀）视为不支持。
    let lang = get("LC_ALL").or_else(|| get("LANG")).unwrap_or_default();
    let c_locale = lang == "C" || lang == "POSIX";
    let unicode = !dumb && !c_locale;

    // italic：Apple Terminal 不支持斜体（降级 dim）；无颜色时 italic 无意义。
    let italic = !no_color && !dumb && get("TERM_PROGRAM").as_deref() != Some("Apple_Terminal");

    // truecolor：COLORTERM=truecolor|24bit 为权威信号；无 COLORTERM 时按
    // TERM_PROGRAM 名单兜底（已知支持 truecolor 的终端）；两者都无信号时
    // 默认假设支持（与 TerminalCaps::default 全能力语义一致）。
    let truecolor = match get("COLORTERM") {
        Some(v) => v == "truecolor" || v == "24bit",
        None => match get("TERM_PROGRAM") {
            None => true,
            Some(prog) => matches!(
                prog.as_str(),
                "iTerm.app"
                    | "WezTerm"
                    | "kitty"
                    | "ghostty"
                    | "vscode"
                    | "tmux"
                    | "Alacritty"
                    | "Hyper"
            ),
        },
    };

    TerminalCaps {
        color: !no_color && !dumb,
        unicode,
        italic,
        truecolor,
        graphics: detect_graphics_protocol_from(get),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 注入式 env 读取器：显式条目优先，缺省回退。
    fn env_with<'a>(
        overrides: &'a [(&'a str, Option<&'a str>)],
    ) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            overrides
                .iter()
                .find(|(k, _)| *k == name)
                .and_then(|(_, v)| v.map(str::to_string))
        }
    }

    #[test]
    fn test_detect_caps_default_full_capability() {
        // 无任何相关 env → 全能力
        let caps = detect_caps_from(&|_| None);
        assert!(caps.color && caps.unicode && caps.italic && caps.truecolor);
    }

    #[test]
    fn test_detect_caps_no_color_presence_disables_color() {
        // NO_COLOR 存在（含空值）即禁用颜色
        for v in [Some("1"), Some(""), Some("true")] {
            let caps = detect_caps_from(&env_with(&[("NO_COLOR", v)]));
            assert!(!caps.color, "NO_COLOR={v:?} 时应禁用颜色");
            // 颜色禁用 → italic 也随之关闭（无颜色时斜体无意义）
            assert!(!caps.italic);
            // 其余能力不受影响
            assert!(caps.unicode && caps.truecolor);
        }
    }

    #[test]
    fn test_detect_caps_dumb_terminal() {
        let caps = detect_caps_from(&env_with(&[("TERM", Some("dumb"))]));
        assert!(!caps.color, "TERM=dumb 禁用颜色");
        assert!(!caps.unicode, "TERM=dumb 降级 ASCII 符号");
        assert!(!caps.italic);
    }

    #[test]
    fn test_detect_caps_colorterm_truecolor() {
        let caps = detect_caps_from(&env_with(&[("COLORTERM", Some("truecolor"))]));
        assert!(caps.truecolor);
        let caps = detect_caps_from(&env_with(&[("COLORTERM", Some("24bit"))]));
        assert!(caps.truecolor);
        let caps = detect_caps_from(&env_with(&[("COLORTERM", Some("256color"))]));
        assert!(!caps.truecolor, "COLORTERM=256color 不是 truecolor");
    }

    #[test]
    fn test_detect_caps_term_program_fallback() {
        // 已知支持 truecolor 的 TERM_PROGRAM → true
        let caps = detect_caps_from(&env_with(&[("TERM_PROGRAM", Some("iTerm.app"))]));
        assert!(caps.truecolor);
        // 未知名单 → false
        let caps = detect_caps_from(&env_with(&[("TERM_PROGRAM", Some("UnknownTerm"))]));
        assert!(!caps.truecolor);
        // Apple Terminal → 无 italic
        let caps = detect_caps_from(&env_with(&[("TERM_PROGRAM", Some("Apple_Terminal"))]));
        assert!(!caps.italic);
        assert!(caps.color && caps.unicode);
    }

    #[test]
    fn test_detect_caps_c_locale_disables_unicode() {
        for lang in ["C", "POSIX"] {
            let caps = detect_caps_from(&env_with(&[("LC_ALL", Some(lang))]));
            assert!(!caps.unicode, "LC_ALL={lang} 降级 ASCII 符号");
        }
        // UTF-8 后缀的 C.UTF-8 仍支持 unicode
        let caps = detect_caps_from(&env_with(&[("LC_ALL", Some("C.UTF-8"))]));
        assert!(caps.unicode);
    }

    #[test]
    fn test_symbols_unicode_set_matches_spec_table() {
        let caps = TerminalCaps {
            color: true,
            unicode: true,
            italic: true,
            truecolor: true,
            graphics: GraphicsProtocol::None,
        };
        let s = symbols(&caps);
        assert_eq!(s.running, "◐");
        assert_eq!(s.success, "✓");
        assert_eq!(s.error, "×");
        assert_eq!(s.warning, "!");
        assert_eq!(s.collapsed, "▸");
        assert_eq!(s.expanded, "▾");
        assert_eq!(s.queued, "·");
        assert_eq!(s.user_prompt, "›");
        // 辅助字形保持 Unicode 原形（§12 契约在 ascii 侧降级）
        assert_eq!(s.todo_started, "▶");
        assert_eq!(s.todo_reopened, "↻");
        assert_eq!(s.todo_edited, "✎");
    }

    #[test]
    fn test_symbols_ascii_fallback_table() {
        // §4.1 降级表：◐✓×!▸▾·› → *+x!>v.>
        let caps = TerminalCaps {
            color: false,
            unicode: false,
            italic: false,
            truecolor: false,
            graphics: GraphicsProtocol::None,
        };
        let s = symbols(&caps);
        assert_eq!(s.running, "*");
        assert_eq!(s.success, "+");
        assert_eq!(s.error, "x");
        assert_eq!(s.warning, "!");
        assert_eq!(s.collapsed, ">");
        assert_eq!(s.expanded, "v");
        assert_eq!(s.queued, ".");
        assert_eq!(s.user_prompt, ">");
        // 辅助字形（§12 同一降级契约）：▶/↻/✎ → >/~/*
        assert_eq!(s.todo_started, ">");
        assert_eq!(s.todo_reopened, "~");
        assert_eq!(s.todo_edited, "*");
        // 显式状态文本后备与符号等价性（无 unicode 时每个语义仍有明确符号）
        let all = [
            s.running,
            s.success,
            s.error,
            s.warning,
            s.collapsed,
            s.expanded,
            s.queued,
            s.user_prompt,
            s.todo_started,
            s.todo_reopened,
            s.todo_edited,
        ];
        assert!(all.iter().all(|c| c.is_ascii()));
    }

    #[test]
    fn test_symbols_all_ascii_distinct_except_documented() {
        // ▸ 与 › 在 ASCII 后备中同为 '>'——规格 §4.1 明确如此降级。
        let s = symbols(&TerminalCaps {
            unicode: false,
            ..TerminalCaps::default()
        });
        assert_eq!(s.collapsed, s.user_prompt);
    }

    #[test]
    fn test_detect_graphics_protocol_peri_image_override() {
        // PERI_IMAGE=off 最高优先级：任何 TERM_PROGRAM 都禁用
        for prog in [Some("kitty"), Some("iTerm.app"), Some("tmux"), None] {
            let caps = detect_graphics_protocol_from(&env_with(&[
                ("PERI_IMAGE", Some("off")),
                ("TERM_PROGRAM", prog),
            ]));
            assert_eq!(
                caps,
                GraphicsProtocol::None,
                "PERI_IMAGE=off 时 TERM_PROGRAM={prog:?} 应禁用"
            );
        }
        // PERI_IMAGE=kitty / iterm2 → 强制对应协议（覆盖品牌映射）
        let caps = detect_graphics_protocol_from(&env_with(&[
            ("PERI_IMAGE", Some("kitty")),
            ("TERM_PROGRAM", Some("UnknownTerm")),
        ]));
        assert_eq!(caps, GraphicsProtocol::Kitty);
        let caps = detect_graphics_protocol_from(&env_with(&[("PERI_IMAGE", Some("iterm2"))]));
        assert_eq!(caps, GraphicsProtocol::ITerm2);
        // 值解析为小写：OFF 也命中 disable
        let caps = detect_graphics_protocol_from(&env_with(&[("PERI_IMAGE", Some("OFF"))]));
        assert_eq!(caps, GraphicsProtocol::None);
        // 未知值 → 安全默认 None；空值视为未设置
        let caps = detect_graphics_protocol_from(&env_with(&[("PERI_IMAGE", Some("halfblock"))]));
        assert_eq!(caps, GraphicsProtocol::None);
        // 空值视为未设置 → 回落品牌映射；品牌映射仅非 Windows 生效
        // （Windows ConPTY 剥 APC，detect 恒 None——见 detect_graphics_protocol_from）
        let caps = detect_graphics_protocol_from(&env_with(&[
            ("PERI_IMAGE", Some("")),
            ("TERM_PROGRAM", Some("kitty")),
        ]));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(caps, GraphicsProtocol::Kitty);
        #[cfg(target_os = "windows")]
        assert_eq!(caps, GraphicsProtocol::None);
    }

    /// Windows 契约：ConPTY 剥 APC（kitty 协议依赖 APC），品牌映射恒 None。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_detect_graphics_protocol_brand_mapping_windows_conpty_disables_all() {
        for prog in [
            "kitty",
            "ghostty",
            "wezterm",
            "warp",
            "iTerm.app",
            "WezTerm",
        ] {
            let caps = detect_graphics_protocol_from(&env_with(&[("TERM_PROGRAM", Some(prog))]));
            assert_eq!(
                caps,
                GraphicsProtocol::None,
                "Windows 上 TERM_PROGRAM={prog} 应恒 None（ConPTY 剥 APC）"
            );
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_detect_graphics_protocol_brand_mapping() {
        // kitty 系品牌 → Kitty（§6.3 E5 矩阵）
        for prog in ["kitty", "ghostty", "wezterm", "warp"] {
            let caps = detect_graphics_protocol_from(&env_with(&[("TERM_PROGRAM", Some(prog))]));
            assert_eq!(
                caps,
                GraphicsProtocol::Kitty,
                "TERM_PROGRAM={prog} 应映射 Kitty"
            );
        }
        // iTerm.app → ITerm2（disabled 语义：能力位记录存在，渲染层视同 None）
        let caps = detect_graphics_protocol_from(&env_with(&[("TERM_PROGRAM", Some("iTerm.app"))]));
        assert_eq!(caps, GraphicsProtocol::ITerm2);
        // 品牌比较大小写不敏感（小写化）
        let caps = detect_graphics_protocol_from(&env_with(&[("TERM_PROGRAM", Some("WezTerm"))]));
        assert_eq!(caps, GraphicsProtocol::Kitty);
    }

    #[test]
    fn test_detect_graphics_protocol_none_fallback() {
        // tmux / 未知 / 无任何 env → None（安全默认，宁可 false negative）
        for prog in [Some("tmux"), Some("UnknownTerm"), None] {
            let caps = detect_graphics_protocol_from(&env_with(&[("TERM_PROGRAM", prog)]));
            assert_eq!(
                caps,
                GraphicsProtocol::None,
                "TERM_PROGRAM={prog:?} 应无协议"
            );
        }
        // TMUX 环境变量存在 → None（即使 TERM_PROGRAM 残留外层终端品牌）
        let caps = detect_graphics_protocol_from(&env_with(&[
            ("TMUX", Some("/tmp/tmux-501/default,123,0")),
            ("TERM_PROGRAM", Some("kitty")),
        ]));
        assert_eq!(caps, GraphicsProtocol::None);
    }

    #[test]
    fn test_detect_caps_graphics_matches_protocol_detection() {
        // 组合断言：detect_caps_from 内联的 graphics 与独立探测函数同源一致。
        let get = env_with(&[("PERI_IMAGE", Some("off")), ("TERM_PROGRAM", Some("kitty"))]);
        assert_eq!(
            detect_caps_from(&get).graphics,
            detect_graphics_protocol_from(&get)
        );
        assert_eq!(detect_caps_from(&get).graphics, GraphicsProtocol::None);

        let get = env_with(&[("TERM_PROGRAM", Some("ghostty"))]);
        assert_eq!(
            detect_caps_from(&get).graphics,
            detect_graphics_protocol_from(&get)
        );
        // 品牌映射仅非 Windows 生效（Windows ConPTY 剥 APC 恒 None）
        #[cfg(not(target_os = "windows"))]
        assert_eq!(detect_caps_from(&get).graphics, GraphicsProtocol::Kitty);
        #[cfg(target_os = "windows")]
        assert_eq!(detect_caps_from(&get).graphics, GraphicsProtocol::None);

        let get = env_with(&[("PERI_IMAGE", Some("iterm2"))]);
        assert_eq!(
            detect_caps_from(&get).graphics,
            detect_graphics_protocol_from(&get)
        );
        assert_eq!(detect_caps_from(&get).graphics, GraphicsProtocol::ITerm2);
    }
}
