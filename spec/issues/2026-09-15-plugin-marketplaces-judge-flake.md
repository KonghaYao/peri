# Plugin Marketplaces 的 Judge 判定首轮失败、重试通过

**状态**：Open
**优先级**：中
**创建日期**：2026-09-15

## 问题

发布门禁 `e2e/results/run-2026-09-15T05-15-49/summary.json` 最终 29/29 文件通过，首轮 28/29。唯一首轮失败是 `e2e/tests/panels/plugin.test.ts:137` 的 `mpResult.pass` 断言，控制面重试后通过；首轮失败数 1 在既定预算 2 内，本次发布门禁 exit 0。

首轮现场 `recordings/worker-2/plugin-marketplaces-tab.txt` 显示 Marketplaces 页面、`claude-plugins-official (cached)`、`github:anthropics/...` 和 `plugins: 1`。仅凭纯文本无法核对 Tab 高亮，也无法区分 Judge 语义误判与无效响应；当前不把它认定为产品故障或某一种 Judge 故障。

## 下一步与验收

- 保留并核对失败时的完整 Judge checks/detail 与 ANSI 样式，明确失败来源。
- 若是判断器故障，保护现有 Tab 激活样式、来源类型与插件数的检查覆盖，修复判定或响应处理。
- 目标单文件通过，并在后续发布门禁核对首轮稳定性；不能只增加重试次数。

## 已完成的相关验收

- 本次 L0：`e2e/results/run-2026-09-15T05-11-49/summary.json`，5/5 首轮通过，exit 0。
- 模型列表名称、宿主 ModelPanel 配置归属与重绘、compact 回放完成提示已修复并经本次全量验证；代码入口见 `docs/code-index/peri-tui.md`。
