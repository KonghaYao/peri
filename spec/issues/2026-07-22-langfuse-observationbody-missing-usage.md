# Langfuse ObservationBody 缺少 `usage` 字段

**状态**：Fixed
**优先级**：中
**类型**：Bug
**创建日期**：2026-07-22

## 问题描述

`langfuse-client` 的 `ObservationBody`（V4 统一观测体，用于 `ObservationCreate`/`ObservationUpdate`）与 Langfuse 最新 OpenAPI spec（`cloud.langfuse.com/generated/api/openapi.yml`, 2026-06-23）不一致，缺少 `usage` 字段。

## 症状详情

- 工具调用（`ObservationType::Tool`）、SubAgent（`ObservationType::Agent`）等通过 V4 `ObservationCreate` 上报时，无法携带 token 用量信息
- `GenerationBody` 已有 `usage` 字段且正确工作，但 `ObservationBody` 漏掉了对应的字段
- 与最新官方 OpenAPI spec 对比确认，`ObservationBody` 应有 `usage: Usage` 字段

## 官方 schema 引用

```
ObservationBody:
  properties:
    ...
    usage:
      $ref: '#/components/schemas/Usage'
      nullable: true

Usage:
  type: object
  properties:
    input: integer (nullable)
    output: integer (nullable)
    total: integer (nullable)
    unit: enum (nullable)
```

## 涉及文件

- `langfuse-client/src/types/mod.rs:128-164` —— `ObservationBody` 定义

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建：对 OpenAPI spec 发现 ObservationBody 缺 usage 字段 |
| 2026-07-22 | Open | Fixed | agent | 修复：ObservationBody 新增 usage 字段，同步补全 3 处构造点 |

## 附注

完整对比中还发现两个非阻塞差异，记录不修：

1. **`ObservationBody` 有额外 `session_id` 字段**（line 163）——官方 spec 无此字段，用于 OTEL `langfuse.session.id` 传播。目前 Langfuse 服务端接受未知字段，无实际影响。
2. **`IngestionEvent` 含 `SessionCreate`/`SessionUpdate`**——最新 spec 的 `oneOf` 列表中无此变体。Session 事件可能已移至独立 API 或在向后兼容模式下继续工作。`deny_unknown_fields` 不在此 enum 上使用，不影响发送。

## 修复记录

### 修复 #1（2026-07-22）

- **操作人**：agent
- **根因**：`ObservationBody` 与最新 Langfuse OpenAPI spec 不一致，缺少 `usage: Usage` 字段
- **修复内容**：
  - `langfuse-client/src/types/mod.rs`：`ObservationBody` 新增 `pub usage: Option<IngestionUsage>`（2 行）
  - `peri-acp/src/langfuse/tracer/mod.rs`：3 处构造点补 `usage: None`（3 行）
  - `langfuse-client/src/types_test.rs`：测试构造点补 `usage: None`（1 行）
- **涉及 commit**：待提交
- **验证状态**：cargo build 通过 / langfuse-client 62 passed / peri-acp 296 passed
