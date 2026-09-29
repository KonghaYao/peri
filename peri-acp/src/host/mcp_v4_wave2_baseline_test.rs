//! MCP adaptation v4-part-3（wave 2）host 侧基线 —— V-01（W0）。
//!
//! **本文件只加观察量，不改行为**：wave 2 要把 cron 的 3 个工具与 lsp 的 1 个工具从
//! 「裸名 deferred 工具」迁为「builtin MCP 实例的 effective name deferred 工具」
//! （`cron_register` → `mcp__cron__cron_register`、`LSP` → `mcp__lsp__LSP`）。迁移前必须
//! 有**可证伪的基线**：本用例在真实生产装配路径上录下「首个 LLM 请求里模型看到的
//! deferred 摘要（system 文本）」中的 cron / lsp 裸名行 —— 同一夹具、同一命令由 wave 2
//! 的收口用例对照重跑，现场输出逐字记录在 wave 2 acceptance 文档中。
//!
//! ## 复用而非复制
//!
//! 宿主（[`WireFixtureHarness`]）、model 替身（[`WireScriptedModel`]）、prompt 驱动
//! （[`run_wire_prompt`]）全部复用 wave 1 的 `host::mcp_v4_wire_fixture`：两个模块同属
//! `host` 树，夹具条目的 `pub(super)` 恰好覆盖本模块，**无需任何可见性放宽**，也不复制
//! 一行实现。
//!
//! ## 两个观察量
//!
//! 1. **wave 1 已录**：首个 LLM 请求的工具名集合（`ModelRequest.tools`）。本用例只复述
//!    同一口径用于现场对照，**不新增断言**，保证 wave 1 的结论不受本次改动影响。
//! 2. **本任务新增**：首个 LLM 请求的 system 文本（ToolSearch deferred 摘要的载体）里
//!    的行数与「含 cron / lsp 裸名」的行。命中判定按 wave 2 规定的四个裸名做**子串**匹配
//!    （`cron_register` / `cron_list` / `cron_remove` / `LSP`），命中行逐字打印供 acceptance
//!    抄录。
//! 3. **结构面（为证伪服务）**：同一文本里 `- <名字>: <描述>` 形态的 deferred **条目名**
//!    清单、`## Deferred Tools` 段标题所在的物理行，以及每个裸名的**载体分布**
//!    （[`bare_name_carriers`]）。没有这一层，第 2 条的「子串命中」无法与「散文 / JSON
//!    工具目录行里的偶发子串」区分 —— 实测正是如此：`collect_prompt_contributions`
//!    （`peri-agent/src/middleware/chain.rs:406`）把各中间件的 contribution **无分隔拼接**
//!    成一条**数万字符**的物理行（末尾接 deferred 段标题），PTC 的 RPC 工具目录 JSON 就
//!    在这条行里，四个裸名会以 `"name":"<裸名>"` 形态全部命中。故**超长命中行按
//!    [`ELIDED_PREFIX_LIMIT`] 截断并显式标注真实长度与命中裸名**（[`VERBATIM_LINE_LIMIT`]
//!    以内仍逐字全量），避免现场证据膨胀到无法抄录，同时不静默丢弃任何信息。
//!
//! ## 为什么用例 1 不断言「裸名一定出现」
//!
//! 这是**迁移前基线**，不是迁移后的不变量：迁移后裸名理应从 deferred 摘要消失。故本用例
//! 只断言「观察面成立」（prompt 正常结束、恰一次模型调用、wire 上真实发生过
//! `initialize` / `tools/list`）与「观察面非空洞」（system 文本非空），**不**对 cron / lsp
//! 是否命中做任何断言 —— 命中与否都是本次要如实记录的事实。
//!
//! ## 用例 2：LSP 裸名的**可观测**面（补的缺口）
//!
//! 用例 1 现场实测：`LSP` 只有 1 次出现、且是**散文 / JSON 子串**（PTC 工具目录），没有
//! 任何 deferred 条目行。原因不在渲染面，而在**注册面**：夹具未配置任何 LSP server，而
//! `LspMiddleware::collect_tools`（`peri-middlewares/src/lsp/middleware.rs:55`）在
//! `LspServerPool::has_servers()` 为 false 时直接返回空 ⇒ `LSP` 工具从未注册。本用例把
//! 这条配置补上，让 `LSP` 在**迁移前**的**摘要面**以裸名 deferred 形态可见
//! （`- LSP: <描述>`）。**注意**：该形态只是**时点证据**（迁移后 effective name 在摘要面
//! 同样缺席，见下一节），A/B 的**等价对照面**是搜索面。
//!
//! **配置来源与注入路径（全部走生产同一函数 / 同一字段，无生产改动）**：
//!
//! 1. 配置源 = 夹具临时 HOME 下 `~/.peri/settings.json` 的 `config.lspServers`
//!    （解析器 `load_global_lsp_config`，`peri-lsp/src/config.rs:71`）；
//! 2. 加载 = 生产函数 `peri_middlewares::assembly::load_merged_lsp_servers`
//!    （宿主调用点 `peri-acp/src/host/assemble.rs`，配置路径取
//!    `crate::provider::config_path()`）；
//! 3. 注入 = **wave 2 起为宿主 pool**：`load_merged_lsp_servers` 的结果喂
//!    `create_host_lsp_pool` → `BuiltinInstanceContext::with_lsp` → `pool.set_builtin_instance_context`
//!    （宿主同一序列见 `peri-acp/src/host/assemble.rs` 与
//!    `peri-acp/src/host/mcp_v4_wave2_test.rs::BuiltinHostFixture::start`），且必须早于
//!    `run_initialize`（A33）。夹具入口
//!    [`super::mcp_v4_wire_fixture::WireFixtureHarness::initialized_with_servers_and_settings`]
//!    把这条链一次做完。
//!
//! **迁移前的注入路径（W0 记录，已不再是现行路径）**：session 字段
//! `SessionContext.lsp_servers` / `lsp_pool`（池构造
//! `create_session_lsp_pool`）。wave 2 的 A21 把工具面归给 builtin `lsp` 实例后，
//! session 字段只剩「门控链上 `LspSyncMiddleware`」的作用，**不再产生 LSP 工具** ——
//! 本文件因此改用上面第 3 条的生产链做实验组；「未注入 ⇒ 无 LSP 工具」的对照由终态
//! 用例 [`super::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary`] §B
//! （空配置 host pool）承担，本文件不重复。
//!
//! **为什么注入必须由夹具自己做**：本 harness 走 `make_session_context` + `run_session_loop`，
//! **不经**宿主装配面（`assemble_server_config` → `session_lifecycle.rs` 的
//! `SessionDeploymentConfig.plugin_lsp_servers`），而 `make_session_context`
//! （`peri-acp/src/host/executor_flow_test.rs`）把 `lsp_servers` 硬编码为空、
//! `lsp_pool` 为空 ⇒ **只写 settings.json 不会让 LSP 工具出现**。
//!
//! 用例 2 的作用是把「LSP 工具在配置存在时确实注册」从不可观测变为可观测：它在**摘要面**
//! 只打印证据（含「是否含裸名 `LSP`」的判断行与原文），**不做**摘要面断言 —— 原因见下一节
//! （摘要面对 `mcp__` 前缀过滤，迁移后 effective name 同样缺席）。LSP 的**时点不变断言**
//! 落在搜索面（XOR）。
//!
//! ## 对照面修订：摘要面**不是**迁移前后的等价对照面，搜索面才是
//!
//! 主线核实（本文件已复读确认）`format_deferred_list`
//! （`peri-middlewares/src/tool_search/tool_index.rs:303`）第 311 行
//! `filter(|(name, _)| !name.starts_with("mcp__"))`：**`## Deferred Tools` 摘要在入口处
//! 整体丢弃 `mcp__` 前缀工具**。由此：
//!
//! 1. 迁移前：裸名 `cron_register` / `cron_list` / `cron_remove` / `LSP` 出现在摘要面
//!    （用例 1/2 已实测）；
//! 2. 迁移后：四者变为 `mcp__cron__*` / `mcp__lsp__LSP`，在该面**整体消失** —— 不是
//!    「换成 effective name 出现」；
//! 3. 故摘要面**只能作时点证据，不能作不变量**：任何「裸名在摘要面」的断言迁移后必红。
//!    本文件已移除该形态的断论（用例 2 的原「断言 2」），摘要面的既有打印逐字保留。
//!
//! **等价对照面 = 搜索面**（`ToolSearchIndex::search` 的检索结果，`tool_index.rs:212`）：
//! 它由 `build()`（`tool_index.rs:190`）按本地工具视图**全量**建立，**不**做 `mcp__` 过滤
//! ⇒ `mcp__` 前缀工具可被检出。现场旁证（用例 1 实测）：`mcp__wire_fixture__glob`
//! （同 server 的 deferred MCP 工具）可被搜索命中，但**不是**摘要面的条目行。
//! 注意该串在 system 文本里另有一次出现，落在 PTC 目录 JSON 那一物理行
//! （`prompt_contribution` 无分隔拼接，与段标题同行）——那是**另一个面**，
//! 故旁证按 [`bare_name_carriers`] 的三分口径（条目行 / `"name":"…"` JSON 行）判定，
//! 不看总出现次数。
//!
//! **探针取 `max_results = 50`（覆盖性，与排序解耦）**：`search()` 只对**必选词**（`+`）
//! 硬过滤，纯关键词查询下所有工具都带分数进入候选，末尾 `sort_by` 降序 + `truncate(limit)`
//! （`:279-284`）。默认 5 会把断言变成「必须排进前 5」，而迁移改的是工具名，排序可能漂移 ——
//! 那会把「工具仍在索引里」误判成假红。取 50 后观察量 = 该 turn 的 deferred 索引清单；
//! 「前 5 名」仍一并打印（保留排序证据，不作断言）。
//!
//! **查询词选定依据（实证，非猜测）**：检索的名称侧打分为
//! `keyword_score`（`peri-middlewares/src/tool_search/keyword_search.rs:81`）里的
//! `name_words`，其构成为 `split_mcp_prefix`（`:40`）+ `split_camel_case`（`:9`）：
//!
//! | 查询 | 迁移前名字 → 名称词 | 迁移后名字 → 名称词 |
//! |---|---|---|
//! | `cron` | `cron_register` → `["cron","register"]` | `mcp__cron__cron_register` → `["cron","cron_register"]` |
//! | `lsp` | `LSP` → `["lsp"]` | `mcp__lsp__LSP` → `["lsp","lsp"]` |
//!
//! 两侧名称词都命中查询词（`words_match`，`:93`），故同一查询在两个时点都能检出该工具，
//! 断言不必因迁移而换查询词。
//!
//! **时点不变断言（XOR）**：对四个工具各断言「裸名与 effective name **恰有其一**出现在
//! 搜索命中名单」：两者都在 = 重复暴露；两者都不在 = 能力净丢失 —— 与 wave 1
//! [`super::mcp_v4_wire_fixture::WEB_ARTIFACT_CAPABILITIES`] 的 XOR 形态同构。
//! 迁移前命中裸名分支、迁移后应命中 effective name 分支，两时点皆绿。
//!
//! **本 XOR 编码的假设（须显式记录）**：迁移后这两个实例的工具仍是**deferred**（进搜索面）。
//! 若 wave 2 把 `mcp__lsp__LSP` 提升为 **direct**，本断言会红 —— 那时正确的处置是**改判对照面**
//! （改用首个请求的直连工具表），**不是**放宽 XOR；故本文件在 XOR 打印里同时给出「直连工具表
//! 命中」一列，使该失败模式一眼可诊。

use std::sync::Arc;

use peri_acp_types::messages::BaseMessage;
use peri_middlewares::assembly::load_merged_lsp_servers;
use peri_middlewares::tool_search::SEARCH_EXTRA_TOOLS_NAME;
use serial_test::serial;

use super::mcp_v4_wire_fixture::{
    run_wire_prompt, ScriptedToolCall, WireFixtureHarness, WireScriptedModel,
    WIRE_FIXTURE_SERVER_NAME,
};
use crate::host::executor_flow_tests::MockEventSink;
use crate::session::executor::{PromptResult, SessionContext};

/// 迁移前 cron / lsp 的四个**裸名**（本基线的命中判据）。
///
/// 只作观察量判据：本文件不对它们做任何策略匹配、归一或改写。
const WAVE2_BARE_NAMES: [&str; 4] = ["cron_register", "cron_list", "cron_remove", "LSP"];

/// 逐字打印上限：`len <= ` 本值即**逐字**全量打印。
const VERBATIM_LINE_LIMIT: usize = 300;

/// 超长行（> [`VERBATIM_LINE_LIMIT`]）打印的前缀长度。
///
/// 为什么需要它：首个 LLM 请求的 system 文本里存在**单行数万字符**的行 —— 各中间件的
/// `prompt_contribution` 被**无分隔拼接**（`peri-agent/src/middleware/chain.rs:406`），
/// PTC 的 RPC 工具目录 JSON 就在其中，裸名会以**散文 / JSON 子串**形态命中它 —— 那不是
/// deferred 条目行。全量逐字打印会让现场证据膨胀到 ~50 KB/行而无法抄录；故超长行打印
/// 「真实长度 + 命中裸名 + 前 [`ELIDED_PREFIX_LIMIT`] 字符」并**显式标注已截断**，
/// 不静默丢弃。
const ELIDED_PREFIX_LIMIT: usize = 200;

/// 单行命中行的现场表述：短行逐字，超长行带长度与截断标注。
fn render_hit_line(line: &str) -> String {
    if line.chars().count() <= VERBATIM_LINE_LIMIT {
        return line.to_string();
    }
    let matched: Vec<&str> = WAVE2_BARE_NAMES
        .iter()
        .copied()
        .filter(|bare| line.contains(bare))
        .collect();
    format!(
        "<过长行已截断: 共 {} 字符；命中裸名 = {matched:?}；前 {ELIDED_PREFIX_LIMIT} 字符 = {}…>",
        line.chars().count(),
        line.chars().take(ELIDED_PREFIX_LIMIT).collect::<String>()
    )
}

/// deferred 条目行的名字（形如 `- <名字>: <描述>`）；非该形态返回 `None`。
///
/// **已知误报类（不修，只标注）**：`format_deferred_list`
/// （`peri-middlewares/src/tool_search/tool_index.rs:322`）把 `tool.description()`
/// **原样**拼进条目行 ⇒ 描述里的内嵌换行会在**列 0** 产生新的 `- <名字>: …` 物理行，
/// 与真条目行同形。实例：`LSP` 的描述内嵌 `Operations:` 列表
/// （`mcp-packages/lsp/src/tool.rs`），10 个操作名会被本函数误判为条目名。
/// 现场可自证：条目列表在真条目上按名字**字典序**（`:313` 的 `sort_by_key`），
/// 被误判的行按**描述内的物理顺序**出现 —— 顺序不一致即说明它们属于同一工具的文本。
fn deferred_entry_name(line: &str) -> Option<&str> {
    line.strip_prefix("- ")?
        .split_once(':')
        .map(|(name, _)| name)
}

/// 一个裸名在同一文本里的载体分布：`(总出现次数, deferred 条目行号, `"name":"<裸名>"` 目录行号)`。
///
/// 三种载体必须分开看：只有**条目行**才是「deferred 摘要里的裸名」；`"name":"…"` 形态来自
/// PTC 的 RPC 工具目录 JSON（`prompt_contribution` 的 `catalog`），是**另一个面**。
fn bare_name_carriers(lines: &[&str], bare: &str) -> (usize, Vec<usize>, Vec<usize>) {
    let json_form = format!("\"name\":\"{bare}\"");
    let mut total = 0;
    let mut entry_lines = Vec::new();
    let mut json_lines = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        total += line.matches(bare).count();
        if deferred_entry_name(line) == Some(bare) {
            entry_lines.push(index);
        }
        if line.contains(&json_form) {
            json_lines.push(index);
        }
    }
    (total, entry_lines, json_lines)
}

// ── 搜索面（迁移前后的**等价**对照面；理由见模块文档）─────────────────────

/// 执行结果里所有 `BaseMessage::Tool` 的文本（`SearchExtraTools` 结果的载体）。
///
/// 链路：`SearchExtraTools::invoke`（`peri-middlewares/src/tool_search/search_tool.rs:88`）
/// 返回 `{"results":[…],"total_available":N}` → react loop 写入 `BaseMessage::Tool`
/// （`peri-acp-types/src/messages/message.rs:97`）→ 出现在
/// `PromptResult.messages`（`peri-acp-types/src/session/execution.rs:388`）。
/// 结果**不截断**：`SearchExtraTools` 未声明 `output_char_limit`（默认 `None`，
/// `peri-acp-types/src/tools.rs:592`），故取到的是索引 `search()` 的原文投影。
fn tool_result_texts(result: &PromptResult) -> Vec<String> {
    result
        .messages
        .iter()
        .filter(|message| matches!(message, BaseMessage::Tool { .. }))
        .map(|message| message.content())
        .collect()
}

/// 从搜索结果的 JSON 原文里取 `results[*].name`（`SearchResult` 序列化形状见
/// `peri-middlewares/src/tool_search/tool_index.rs:17-23`）。
fn searched_result_names(texts: &[String]) -> Vec<String> {
    texts
        .iter()
        .filter_map(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .filter_map(|value| value.get("results")?.as_array().cloned())
        .flatten()
        .filter_map(|result| {
            result
                .get("name")
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .collect()
}

/// 搜索面探针的观察量：该次运行**首个请求的直连工具名** + **搜索结果工具名**。
struct SearchFace {
    /// 首个 LLM 请求的直连工具表（迁移后若工具被提升为 direct，本列会亮 —— 见模块文档）。
    direct: Vec<String>,
    /// `SearchExtraTools` 结果的 `results[*].name`（含 `mcp__` 前缀工具）。
    hits: Vec<String>,
}

/// 搜索面探针：脚本化调用一次 `SearchExtraTools`，返回命中的工具名与直连表。
///
/// 每次探针 = 1 次工具调用 + 1 次收尾，故**恰好 2 次模型调用**（与既有用例的
/// 「恰好 1 次」断言互不干扰：各用各自的 model 实例）。
///
/// **`max_results = 50` 的用意（与检索实现绑定的事实）**：`search()`
/// （`peri-middlewares/src/tool_search/tool_index.rs:212`）只在存在**必选词**（查询里带 `+`）
/// 时硬过滤；纯关键词查询下每个工具都会带上分数进入候选，最后 `sort_by` 降序 +
/// `truncate(limit)`（`:284`）。默认 `limit = 5` 会把断言变成「必须排进前 5」——那依赖**排序**，
/// 而迁移会改工具名（`cron_register` → `mcp__cron__cron_register`），排序可能漂移，会把
/// 「工具仍在索引里」误判成红。取 50 后 `truncate` 不生效，观察量 = **deferred 索引清单**
/// （覆盖性），与排序解耦；同时把「前 5 名」一并打印，保留排序证据（不作断言）。
async fn search_face_probe(label: &str, ctx: SessionContext, query: &str) -> SearchFace {
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![ScriptedToolCall::new(
        SEARCH_EXTRA_TOOLS_NAME,
        serde_json::json!({ "query": query, "max_results": 50 }),
    )]));
    let result = run_wire_prompt(ctx, &sink, &model).await;

    assert!(
        result.ok,
        "搜索面 {label}：探针 prompt 必须正常结束: stop={:?}",
        result.stop_reason
    );
    assert_eq!(
        model.call_count(),
        2,
        "搜索面 {label}：一次工具调用 + 一次收尾 = 恰好 2 次模型调用"
    );

    let texts = tool_result_texts(&result);
    let hits = searched_result_names(&texts);
    let top5: Vec<&String> = hits.iter().take(5).collect();
    // 打印口径：「命中工具名」= 索引清单（**成员判定**，与次序无关，断言只用它）；
    // 「前 5 名」= 排序证据 —— 低分并列项的次序**不稳定**（候选来自 HashMap 迭代，
    // `sort_by` 对并列项保持输入序，而 HashMap 次序每进程不同），故只作参考、不作断言。
    println!(
        "[W2-V01 基线] 搜索面 {label}：查询 = {query:?}（max_results = 50）；tool 结果消息数 = {}；结果原文长度 = {} 字符；\
         命中工具名（= 该 turn 的 deferred 索引清单）= {hits:?}；前 5 名 = {top5:?}",
        texts.len(),
        texts.iter().map(|text| text.chars().count()).sum::<usize>()
    );

    SearchFace {
        direct: model.first_request_tool_names(),
        hits,
    }
}

/// **时点不变断言**：`bare` 与 `effective` **恰有其一**出现在搜索面命中名单里。
///
/// 迁移前本地工具视图只有裸名工具、迁移后只有 `mcp__…` 工具，故「恰有其一」在两个时点都成立；
/// 「两者都在」= 重复暴露、「两者都不在」= 能力净丢失，都被本断言挡下。同时打印「直连工具表
/// 命中」一列：迁移后若该工具被提升为 direct，本断言会红，而该列立刻指出原因（见模块文档
/// 的「本 XOR 编码的假设」）。
fn assert_name_xor(label: &str, face: &SearchFace, bare: &str, effective: &str) {
    let bare_hit = face.hits.iter().any(|name| name == bare);
    let bare_direct = face.direct.iter().any(|name| name == bare);
    let effective_hit = face.hits.iter().any(|name| name == effective);
    let effective_direct = face.direct.iter().any(|name| name == effective);
    println!(
        "[W2-V01 基线] 搜索面 {label}：裸名 `{bare}` → 搜索结果命中 = {bare_hit}、直连工具表命中 = {bare_direct}；\
         effective name `{effective}` → 搜索结果命中 = {effective_hit}、直连工具表命中 = {effective_direct}"
    );
    assert!(
        bare_hit ^ effective_hit,
        "搜索面 {label}：`{bare}` 与 `{effective}` 必须恰有其一（都在 = 重复暴露；都不在 = 能力净丢失）：\
         搜索命中 = {:?}；直连工具表 = {:?}",
        face.hits,
        face.direct
    );
}

/// **W0 基线**：首个 LLM 请求 + deferred 摘要里的 cron / lsp 裸名行。
///
/// 现场证据（`--nocapture` 逐字抄录）：
/// - `[W2-V01 基线] 首个 LLM 请求工具数 = N；工具名 = [...]`（wave 1 观察量，复述）；
/// - `[W2-V01 基线] deferred 摘要行数 = M；cron/lsp 命中行 = [...]`（本任务新增）；
/// - 随后每行命中行以 `[W2-V01 基线] 命中行[i] 原文: <行>` 逐字打印。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn wave2_baseline_first_request_and_deferred_summary() {
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![]));
    let result = run_wire_prompt(
        harness.session_context("mcp-v4-wave2-baseline").await,
        &sink,
        &model,
    )
    .await;

    // ── 观察面成立（与迁移语义无关的三条） ────────────────────────────────
    assert!(
        result.ok,
        "准入成功后 prompt 必须正常结束: stop={:?}",
        result.stop_reason
    );
    assert_eq!(model.call_count(), 1, "首个 prompt 恰好一次模型调用");

    // ── 观察量 1（wave 1 口径，本用例只复述） ─────────────────────────────
    let names = model.first_request_tool_names();
    println!(
        "[W2-V01 基线] 首个 LLM 请求工具数 = {}；工具名 = {names:?}",
        names.len()
    );

    // ── 观察量 2（本任务新增）：deferred 摘要里的 cron / lsp 裸名行 ────────
    let system_text = model.first_request_system_text();
    let lines: Vec<&str> = system_text.lines().collect();
    let hits: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| WAVE2_BARE_NAMES.iter().any(|bare| line.contains(bare)))
        .map(|(index, line)| (index, *line))
        .collect();
    let listed: Vec<String> = hits
        .iter()
        .map(|(index, line)| format!("#{index}={}", render_hit_line(line)))
        .collect();
    println!(
        "[W2-V01 基线] deferred 摘要行数 = {}；cron/lsp 命中行 = {listed:?}",
        lines.len()
    );
    for (index, line) in &hits {
        println!(
            "[W2-V01 基线] 命中行[{index}] 原文: {}",
            render_hit_line(line)
        );
    }

    // 结构面：`- <名字>: <描述>` 形态的 deferred 条目名，用于把「条目命中」与
    // 「散文 / JSON 目录行里的子串误命中」分开（现场证据的可证伪性依赖这一层）。
    let entry_names: Vec<&str> = lines
        .iter()
        .filter_map(|line| deferred_entry_name(line))
        .collect();
    println!(
        "[W2-V01 基线] deferred 条目名（`- <名字>:` 形态，共 {} 个）= {entry_names:?}",
        entry_names.len()
    );
    println!(
        "[W2-V01 基线] `## Deferred Tools` 段标题所在物理行 = {:?}",
        lines
            .iter()
            .position(|line| line.contains("## Deferred Tools"))
    );

    // 载体分布：把「deferred 条目行里的裸名」与「PTC RPC 目录 JSON 里的裸名」分开 ——
    // 后者不是 deferred 摘要，命中它属于子串误命中。
    for bare in WAVE2_BARE_NAMES {
        let (total, entry_lines, json_lines) = bare_name_carriers(&lines, bare);
        println!(
            "[W2-V01 基线] 裸名 `{bare}`：总出现 = {total} 次；deferred 条目行 = {entry_lines:?}；\
             `\"name\":\"{bare}\"` 目录 JSON 行 = {json_lines:?}"
        );
    }

    // 观察面非空洞：system 文本为空时「命中行 = []」是无意义的假阴性。
    assert!(
        !lines.is_empty(),
        "首个 LLM 请求的 system 文本不得为空（否则本基线观察量空洞）"
    );

    // ── wire 事实：本轮确实经真实 MCP 初始化 ─────────────────────────────
    let methods: Vec<String> = harness
        .wire_received_requests()
        .iter()
        .filter_map(|request| request["method"].as_str().map(str::to_string))
        .collect();
    assert!(
        methods.iter().any(|method| method == "initialize"),
        "夹具必须真实收到 initialize: {methods:?}"
    );
    assert!(
        methods.iter().any(|method| method == "tools/list"),
        "夹具必须真实收到 tools/list: {methods:?}"
    );

    // ── 搜索面（时点不变对照面，见模块文档）：cron 三工具的 XOR ──────────────
    // 本用例不注入 LSP 配置（LSP 的 XOR 在用例 2）。
    let cron_face = search_face_probe(
        "cron",
        harness
            .session_context("mcp-v4-wave2-baseline-cron-search")
            .await,
        "cron",
    )
    .await;
    assert_name_xor(
        "cron",
        &cron_face,
        "cron_register",
        "mcp__cron__cron_register",
    );
    assert_name_xor("cron", &cron_face, "cron_list", "mcp__cron__cron_list");
    assert_name_xor("cron", &cron_face, "cron_remove", "mcp__cron__cron_remove");

    // ── 旁证：搜索面收录 `mcp__` 前缀工具，而摘要面整体过滤它们 ──────────────
    // `mcp__wire_fixture__glob` 是同 server（`wire_fixture`）的 deferred MCP 工具：
    // 它在搜索面可检出，但**绝不**成为 `## Deferred Tools` 的条目行（`tool_index.rs:311`）。
    // 注意：该字符串在 system 文本里**另有 1 次**出现，落在 PTC 目录 JSON 那一物理行上
    // （`prompt_contribution` 无分隔拼接，与段标题同行，属**另一个面**）——故这里按
    // [`bare_name_carriers`] 的三分口径区分「条目行」与「`"name":"…"` JSON」，不看总次数。
    let (glob_total, glob_entry_lines, glob_json_lines) =
        bare_name_carriers(&lines, "mcp__wire_fixture__glob");
    println!(
        "[W2-V01 基线] 搜索面 旁证：`mcp__wire_fixture__glob` 在摘要面总出现 = {glob_total} 次；\
         其中 `- <名字>:` 条目行 = {glob_entry_lines:?}；`\"name\":\"…\"` 形态（PTC 目录 JSON）行 = {glob_json_lines:?}"
    );
    assert!(
        glob_entry_lines.is_empty(),
        "`mcp__` 前缀工具不得成为 `## Deferred Tools` 条目行（`tool_index.rs:311` 的整体过滤）: {glob_entry_lines:?}"
    );
    let glob_face = search_face_probe(
        "glob（旁证：mcp__ 前缀工具只在搜索面）",
        harness
            .session_context("mcp-v4-wave2-baseline-glob-search")
            .await,
        "glob",
    )
    .await;
    assert!(
        glob_face
            .hits
            .iter()
            .any(|name| name == "mcp__wire_fixture__glob"),
        "搜索面必须收录 `mcp__` 前缀的 deferred 工具（这是「搜索面可作迁移后对照面」的机制旁证）: {:?}",
        glob_face.hits
    );
}

// ── 用例 2：LSP server 配置存在时的裸名可观测性 ─────────────────────────────

/// 最小 LSP server 配置（用例 2 写入夹具临时 HOME 的 `~/.peri/settings.json`）。
///
/// 形状对齐生产解析器 `load_global_lsp_config`（`peri-lsp/src/config.rs:71`）：顶层
/// `config.lspServers` 为 `{ <server 名>: LspServerConfig }`；`name` 以 **key** 为准
/// （`peri-lsp/src/config.rs:97`），故此处不写 `name` 字段。`command` 故意指向**不存在的
/// 可执行文件**：注册面只读配置表 —— `LspServerPool::new`（`peri-lsp/src/pool.rs:47`）
/// 是**惰性构造**（只建 `LspClient` 结构体，不 spawn），`has_servers()`
/// （`peri-lsp/src/pool.rs:223`）= `!servers.is_empty()`；即便某条路径真的尝试拉起，
/// 也会 ENOENT 立即失败，不会把用例挂在启动超时上。扩展名映射用探针后缀，
/// 不覆盖任何真实文件类型（不干扰 Write/Edit 的诊断路由）。
const W2_LSP_SETTINGS_JSON: &str = r#"{
  "config": {
    "lspServers": {
      "wave2_probe": {
        "command": "/nonexistent/peri-wave2-lsp-probe",
        "args": ["--probe"],
        "extensionToLanguage": { ".w2probe": "plaintext" }
      }
    }
  }
}"#;

/// 打印一条 prompt 运行在「首个 LLM 请求」上的 deferred 面（与用例 1 同口径、带用例标签），
/// 返回 `- <名字>:` 形态的 deferred **条目名**供调用方断言。
///
/// 与用例 1 的逐字打印使用同一批辅助函数（[`render_hit_line`] / [`deferred_entry_name`] /
/// [`bare_name_carriers`]）；用例 1 的打印**逐字不动**（其现场证据已被 acceptance 抄录）。
fn print_deferred_face(label: &str, model: &WireScriptedModel) -> Vec<String> {
    let tool_names = model.first_request_tool_names();
    println!(
        "[W2-V01 基线] {label}：首个 LLM 请求工具数 = {}；工具名 = {tool_names:?}",
        tool_names.len()
    );

    let system_text = model.first_request_system_text();
    let lines: Vec<&str> = system_text.lines().collect();
    let hits: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| WAVE2_BARE_NAMES.iter().any(|bare| line.contains(bare)))
        .map(|(index, line)| (index, *line))
        .collect();
    let listed: Vec<String> = hits
        .iter()
        .map(|(index, line)| format!("#{index}={}", render_hit_line(line)))
        .collect();
    println!(
        "[W2-V01 基线] {label}：deferred 摘要行数 = {}；cron/lsp 命中行 = {listed:?}",
        lines.len()
    );

    let entry_names: Vec<String> = lines
        .iter()
        .filter_map(|line| deferred_entry_name(line))
        .map(str::to_string)
        .collect();
    println!(
        "[W2-V01 基线] {label}：deferred 条目名（`- <名字>:` 形态，共 {} 个）= {entry_names:?}",
        entry_names.len()
    );
    // 带**物理行号**的同口径清单：让「真条目行（按名字字典序）」与「描述内嵌 bullet
    // （按描述内物理顺序，与字典序不一致）」的区别在现场可见（见 deferred_entry_name 文档）。
    let indexed: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| deferred_entry_name(line).map(|name| (index, name)))
        .collect();
    println!("[W2-V01 基线] {label}：deferred 条目行（行号@名字）= {indexed:?}");

    // 本用例的判据：裸名 `LSP` 是否**以 deferred 条目行**出现（含则打印该行原文）。
    match lines
        .iter()
        .copied()
        .find(|line| deferred_entry_name(line) == Some("LSP"))
    {
        Some(line) => {
            println!("[W2-V01 基线] {label}：deferred 条目含裸名 `LSP` = 是；原文 = {line}");
        }
        None => println!(
            "[W2-V01 基线] {label}：deferred 条目含裸名 `LSP` = 否（条目名 = {entry_names:?}）"
        ),
    }

    // 载体分布（与用例 1 同口径）：把「deferred 条目行」与「PTC 目录 JSON 子串」分开。
    for bare in WAVE2_BARE_NAMES {
        let (total, entry_lines, json_lines) = bare_name_carriers(&lines, bare);
        println!(
            "[W2-V01 基线] {label}：裸名 `{bare}`：总出现 = {total} 次；deferred 条目行 = {entry_lines:?}；\
             `\"name\":\"{bare}\"` 目录 JSON 行 = {json_lines:?}"
        );
    }

    entry_names
}

/// **W0 基线（用例 2）**：配置了 LSP server 时，LSP 工具在**搜索面**可检出。
///
/// **口径修订（wave 2 后，显式登记）**：迁移前的注入路径是 session 字段
/// `SessionContext.lsp_servers` / `lsp_pool`（`create_session_lsp_pool`），本用例据此
/// 造出「未注入 ⇒ 无 LSP 条目」的对照；A21 把工具面归给 builtin `lsp` 实例后该路径不再
/// 产生工具，实验组与搜索探针改走**生产链**
/// （settings.json → `load_merged_lsp_servers` → `create_host_lsp_pool` → 上下文注入 →
/// `run_initialize`，见 [`WireFixtureHarness::initialized_with_servers_and_settings`]）。
/// 断言（搜索面 XOR）不变；「未注入 ⇒ 无 LSP 工具」的对照改由终态用例
/// [`super::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary`] §B 承担
/// （同一 session 内不得并存两个夹具：HOME 守卫不可重入）。
///
/// 现场证据（`--nocapture` 逐字抄录，`{label}` 见打印行）：
/// - `[W2-V01 基线] LSP 用例：settings 路径 = …；生产加载函数解析出 server 数 = 1；server 名 = [...]`；
/// - `[W2-V01 基线] LSP 用例（host 注入）：首个 LLM 请求工具数 = N；工具名 = [...]`；
/// - 搜索面 XOR 打印行（[`assert_name_xor`]）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn wave2_baseline_lsp_tool_visible_when_server_configured() {
    // 配置在**池构造之前**写入并解析（A33 的注入时序，见夹具文档）。
    let harness =
        WireFixtureHarness::initialized_with_servers_and_settings(&[], Some(W2_LSP_SETTINGS_JSON))
            .await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    // ── 配置来源：夹具临时 HOME 下的 `~/.peri/settings.json` ────────────────
    // 路径来源与宿主装配面同一函数（`crate::provider::config_path()`）
    let settings_path = crate::provider::config_path();
    let home = std::env::var("HOME").expect("夹具必须已把 HOME 重定向到临时目录");
    assert!(
        settings_path.starts_with(&home),
        "LSP 配置必须落在夹具临时 HOME 内（否则写的是开发者本机配置）: {}（HOME={home}）",
        settings_path.display()
    );

    // ── 生产加载函数读回（与宿主装配面同一函数、同一路径来源）──────────────
    // 注入用的就是这次解析的结果（夹具构造期跑同一函数），此处独立复核解析面。
    let merged = load_merged_lsp_servers(&settings_path, Vec::new());
    let server_names: Vec<&str> = merged.iter().map(|server| server.name.as_str()).collect();
    let injected_names: Vec<&str> = harness
        .merged_lsp_servers()
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    println!(
        "[W2-V01 基线] LSP 用例：settings 路径 = {}；生产加载函数解析出 server 数 = {}；server 名 = {server_names:?}；\
         注入 host pool 的 server 名 = {injected_names:?}",
        settings_path.display(),
        merged.len()
    );
    assert_eq!(
        merged.len(),
        1,
        "settings.json 必须解析出恰好 1 个 LSP server（否则用例前提不成立）: {server_names:?}"
    );
    assert_eq!(
        injected_names, server_names,
        "注入 host pool 的配置必须逐条等于生产加载函数对同一文件的解析结果"
    );

    // ── 实验组：host 注入（A33）⇒ 工具面按生效配置出现 ─────────────────────
    let ctx = harness.session_context("mcp-v4-wave2-baseline-lsp").await;
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![]));
    let result = run_wire_prompt(ctx, &sink, &model).await;

    // ── 观察面成立（与用例 1 同一口径）────────────────────────────────────
    assert!(
        result.ok,
        "host 注入 lsp 配置后 prompt 必须正常结束: stop={:?}",
        result.stop_reason
    );
    assert_eq!(model.call_count(), 1, "host 注入后恰好一次模型调用");

    print_deferred_face("LSP 用例（host 注入）", &model);

    // ── 摘要面**不再断言**（对照面修订）───────────────────────────────────
    // 原「断言 2：`LSP` 必须以裸名出现在 deferred 摘要条目中」是**迁移后必红**的时点断言，已移除：
    // `format_deferred_list` 在入口过滤 `mcp__` 前缀（`tool_index.rs:311`）⇒ 迁移后
    // `mcp__lsp__LSP` 同样**不在**摘要面（不是换名出现）。故摘要面只作时点证据
    // （上面的 `print_deferred_face` 已逐字打印，含「是否含裸名 `LSP`」判断行与原文），
    // 不变量落在下面的搜索面 XOR。该面只作时点证据，返回值不再参与断言。
    //
    // 同理，迁移前那句「只写 settings.json、不注入 ⇒ 无 LSP 条目」的对照**已随注入路径变更
    // 移除**（同一夹具只有一个 host pool，只能注入一次）：该负面事实现由终态用例
    // `host::mcp_v4_wave2::wave2_final_first_request_and_deferred_summary` §B 承担 ——
    // 空配置 host pool 下实例仍 ready、搜索面不含 `mcp__lsp__*`。

    // ── 搜索面（时点不变对照面，见模块文档）：LSP 的 XOR ────────────────────
    // session 字段已不承载 LSP 工具面（A21），故探针只投影同一 host pool。
    let lsp_face = search_face_probe(
        "lsp",
        harness
            .session_context("mcp-v4-wave2-baseline-lsp-search")
            .await,
        "lsp",
    )
    .await;
    assert_name_xor("lsp", &lsp_face, "LSP", "mcp__lsp__LSP");

    // ── wire 事实：本轮确实经真实 MCP 初始化（与用例 1 同口径）────────────
    let methods: Vec<String> = harness
        .wire_received_requests()
        .iter()
        .filter_map(|request| request["method"].as_str().map(str::to_string))
        .collect();
    assert!(
        methods.iter().any(|method| method == "initialize"),
        "夹具必须真实收到 initialize: {methods:?}"
    );
    assert!(
        methods.iter().any(|method| method == "tools/list"),
        "夹具必须真实收到 tools/list: {methods:?}"
    );
}
