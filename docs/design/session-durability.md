# Peri Durable：进入会话时恢复

- 状态：**已批准目标设计**；实现状态以代码和验收为准。
- 范围：主会话与异步子 Agent 的按需续跑、MQ 恢复、context 占用展示、外围存活任务发现。
- 仓库文档位置：`docs/design/session-durability.md`。
- 前置设计：[Thread MQ](https://chatgpt.com/library?file_id=libfile_522d76ed8ac88191ac4ad25bd0909ca5&account_id=personal)。先完成 MQ 重构，再落实本文。
- MQ 文档中留待后续决定的结果处理恢复范围，以本文第 7 节为准：本版本不持久化逐条处理记录。

## 1. 恢复契约

1. Peri 启动、会话列表、历史分页和只读查询不启动恢复执行。只有进入某个会话，才判断其根 thread 范围内的恢复需求。
2. 以已有 `thread_id`、`parent_thread_id` 和持久工作意图选择恢复对象。根 thread 只确定范围，不是所有结果的统一收件人。
3. 主会话及其范围内需要继续工作的 async Agent 使用原 thread 恢复历史、MQ 和配置，再注入恢复提醒并继续执行。
4. sync Agent 不自动续跑；按中断语义结束遗留执行，保留显式 resume 能力。已经实际转为后台继续执行的委派按 async 处理；仅停驻资源等待显式 resume 的 child 不自动复活。
5. TaskManager、运行锁、取消令牌、订阅和当前执行身份在本进程重新建立。稳定恢复身份是 thread，不要求复原旧内存 `task_id`、request ID 或回调闭包。
6. 读取已提交事实后开始新的运行，不重放旧工具调用、发送命令或停机期间的外围事件。
7. MQ 只保存待发送消息；历史消息仍归 `messages`。不新增异步任务绑定表或逐条结果处理标记。

本设计恢复线程树内的工作与当前运行登记。旧进程任意自定义闭包的恢复不属于承诺范围；正常装配仍遵循现有会话绑定、配置回调优先级和直接接收会话规则。

原投递目标必须能由现有持久关系与配置、经既有装配规则重新确定。无法确定时，该 child 报告恢复失败，不猜测接收人或回退到 root；也不为此新增持久绑定数据。

## 2. SQLite：会话表只新增两列

```sql
ALTER TABLE threads
ADD COLUMN context_usage_tokens INTEGER
CHECK (
    context_usage_tokens IS NULL
    OR context_usage_tokens >= 0
);

ALTER TABLE threads
ADD COLUMN work_state TEXT NOT NULL DEFAULT 'idle'
CHECK (work_state IN ('idle', 'working', 'stopped'));
```

| 字段 | 保存内容 |
| --- | --- |
| `threads.context_usage_tokens` | 最近一次有效上下文占用的 token 数；`NULL` 表示未知，不是零占用。 |
| `threads.work_state` | durable 工作意图；`working` 允许进入会话后自动续跑，`idle` 不自动续跑，`stopped` 保留显式停止意图。 |

既有 `id`、`parent_thread_id`、`cancel_policy`、`agent_status`、`config`、frozen/inherited context 继续使用。MQ 表结构由 Thread MQ 文档维护，不在这里再建立一份消息存储。

不创建 `agent_task_bindings`，不增加 `messages.awaiting_response`，不新增 pending-result ID 集合、命令表、执行阶段表或恢复检查点。

### 2.1 `work_state` 与实时状态

`work_state` 表示中断后的续跑意图，不承担 TUI loading 或 TaskManager 实时状态的职责。运行中是否忙碌、是否正在等待，由现有运行态表达。

| 操作 | `work_state` 的目标值 |
| --- | --- |
| 主会话执行获得准入 | `working` |
| async child 开始执行，或 sync 委派实际转为后台继续执行 | `working` |
| sync child 开始执行，本次执行不纳入自动恢复 | `idle`；现有 `agent_status`／运行事件仍正常表达它正在工作 |
| Receive 将输入移交历史 | 保持当前继续工作意图，不因 MQ 行消失而写 `idle` |
| 正常完成、明确失败、或交还控制等待显式 resume | `idle`，并维护既有终态 |
| 用户 Stop | 受现有取消策略影响的对象写 `stopped` |
| 用户明确继续或发起允许解除停止的新执行 | 在获得准入时写 `working`；sync child 仍按上面的规则 |
| 进程异常退出 | 不写终态，保留最后提交的值 |

sync 无须新增持久 run-mode 字段。自动恢复只选择 `working`；业务入口负责写对续跑意图，不从 `cancel_policy` 猜同步／异步模式。

旧数据的新增列初始化为 `idle` 和 `NULL`。旧 `agent_status = 'active'` 不构成自动恢复资格，不批量唤醒历史会话。

### 2.2 Context 占用

- 保存现有占用计算产生的有效 token 数，不保存累计计费输入量或完整 TokenTracker。
- 进入会话时先显示已保存值；百分比使用当前有效配置中的 context window 计算。
- 占用变化后更新这一列，语义是最后提交的展示快照，不承诺逐个流式 token 落库。
- Compact、rewind 或模型配置变化使旧值失效时，更新新有效值；暂时无法获得有效值则写 `NULL`。
- 新执行的预算判断基于重建的有效上下文和现有预算逻辑，不能把旧展示快照当作本次模型请求的精确 usage。

## 3. 唯一恢复时机与范围

| 事件 | 恢复执行 |
| --- | --- |
| Peri 进程启动 | 否 |
| 会话列表、历史分页、只读元数据查询 | 否 |
| 进入主会话 A | 检查 A 所在根 thread 的范围 |
| 进入同一根下可执行的子会话 | 解析同一根范围，复用其运行协调 |
| 再次进入已运行的 A | 连接当前实例，不重复启动或重复注入恢复提醒 |
| 切换到 B | 检查 B，不顺手停止、重启或恢复 A 的其他工作 |
| 本进程尚未进入的会话收到消息 | 可以持久接纳 MQ，不因此冷启动 Agent |

进入是业务上的会话激活动作，不能把所有名为 load 的读取方法都当成恢复入口。历史展示加载必须与可执行会话进入分开。

先沿已有父链找到根，并校验缺失节点、循环和环境绑定。之后只读取这个根的 thread 子树。SQL 选择形态如下：

```sql
WITH RECURSIVE thread_scope(id) AS (
    SELECT id
    FROM threads
    WHERE id = :root_thread_id

    UNION

    SELECT child.id
    FROM threads AS child
    JOIN thread_scope AS parent
      ON child.parent_thread_id = parent.id
)
SELECT t.id, t.parent_thread_id, t.work_state, t.context_usage_tokens
FROM threads AS t
JOIN thread_scope AS scope ON scope.id = t.id
WHERE t.work_state = 'working';
```

这条查询确定候选，不直接授权启动。主会话不是 `working` 时，仍检查同范围内各 child 的自身状态；为 child 重建必要的父级收件地址，不意味着启动父级模型。

只读 Store、不可执行环境或损坏关系不能放行执行；已能读取的历史与占用仍可展示，恢复错误显式报告。会话装配与历史回放成功后，恢复准备失败按「部分成功」返回：`session/load|resume` 仍为 Ok，结果写入 `_meta.peri.recovery`；使会话不可执行的失败（`Corrupt` / `ReadOnly` / `NotFound`）另以 `-32017` 拒绝 prompt。只读 Store 下的 `session/load` 仍在回放前明确拒绝（`ReadOnlyStore`）；经 `-r` 浏览只读历史本版本不提供，需独立的只读视图会话生命周期，不能用小补丁伪装成可重试。

## 4. 恢复流程

```mermaid
flowchart TD
    E["进入会话"] --> R["解析根 thread 与恢复范围"]
    R --> S["加载范围内的历史、MQ、配置和工作意图"]
    S --> B["建立父子收件地址与运行登记"]
    B --> G{"逐个 thread：实例已运行？"}
    G -->|"是"| A["连接现有实例"]
    G -->|"否，且为恢复候选"| X["按直接 owner 查询外围存活任务"]
    X --> C{"逐个复核 working 与准入"}
    C -->|"允许"| N["恢复提醒入本 thread MQ"]
    N --> L["进入现有 Agent Loop"]
    C -->|"停止、空闲或失效"| V["保留数据与展示，不启动执行"]
```

1. 同一根范围内的重复进入请求合并；与正常启动、Stop、关闭、显式 resume 共用现有运行协调。
2. 加载所需 thread 的有效历史、frozen/inherited context、MQ 与配置，不重新冻结已有上下文。
3. 先建立收件地址与父子关系，再放行 child。嵌套 `A → B → C` 的消息保持对应会话归属，不能统一送到 A。
4. 对候选 async child 建立当前进程的 TaskManager 登记，保留原 child thread。不会新建一个替代 child，也不会重复执行旧的 Agent 工具调用。
5. 按各 thread 的直接 owner 查询外围任务现状并建立当前监听。结果供恢复提醒及任务展示使用，不复制为 Peri 的外围任务状态表。
6. 异步准备结束后，重新读取工作意图并检查当前实例与执行资格。快照之后发生的 Stop、关闭和手动 resume 必须生效。
7. 向本次实际需要续跑的 thread MQ 注入可信恢复提醒，再由现有 Receive 与 Loop 继续。提醒不是新的用户委派，不自动启动 `idle` 或 `stopped` 会话。

恢复协调、正在准备的集合和重复进入合并仅存在于本进程。重复进入不能创建第二个 worker，也不能把旧准备结果发布到新的会话生命周期。

根 thread 已经运行，不代表可以跳过其他 child 的检查。上图逐个处理范围内的 thread；已有实例复用原历史投影、收件地址与登记，不能用磁盘快照覆盖它。

### 4.1 恢复提醒

提醒说明发生过进程中断、现在从已提交历史继续，以及直接归属该 thread 的存活 Agent／Shell 任务。任务列表由当前事实生成，不读取停机期间事件日志。

示例正文：

```text
会话从中断后的已提交历史继续。旧工具调用不会自动重放。
本会话当前存活的任务：
- Agent child-a1：working，已建立本次运行登记。
- Shell shell-42：running，由 Workspace 查询确认。
未取得结果的外部调用保持未知，请依据当前任务状态继续处理。
```

通过现有可信 SystemReminder 入口创建，经过 MQ 持久接纳和 Receive 移交；不把此示例文本本身当成权限凭证。UI、模型与诊断分别遵循既有受众与区域过滤。

提醒绑定本次有效执行。MQ 中针对旧运行的控制消息沿 Thread MQ 的失效绑定规则结算，不把所有旧消息统一改绑到新运行。

### 4.2 Rust 恢复入口

在现有会话生命周期服务中提供统一业务入口，接口契约如下；它不是第二套 Store，也不向调用方开放任意状态写入：

```rust
#[async_trait::async_trait]
pub trait SessionRecovery: Send + Sync {
    async fn enter(
        &self,
        thread_id: &ThreadId,
    ) -> Result<RecoveryReport, RecoveryError>;
}
```

`RecoveryReport` 返回本次根 thread、复用的实例、实际恢复的 thread、保持不运行的 thread 及逐项失败。它是调用结果，不持久化为恢复记录表。正常只读加载不调用 `enter`。

进入完成后，Host 把报告投影为 `RecoveryView`（`peri_acp_types::recovery`）并附加到 `session/load|resume` 响应的 `_meta.peri.recovery`；仅在本次实际恢复或失败时附加。`Failed` 且 `executionBlocked` 的失败在会话 runtime 上留下内存执行阻断，`session/prompt` 统一返回 `-32017` + `data.reason = "recovery_execution_blocked"`；`Unavailable` / `Superseded` 保持可重试，不阻断显式继续。阻断状态不持久化，每次新的 load/resume 重新判定。

冷 child 的 `assemble_for_recovery` 只建立运行实例；调用它的 server loop 必须在响应前补齐 TaskManager 绑定与在线 endpoint。`discard` 只丢弃 `Prepared` 启动资格，MQ 中的提醒保留，下一次进入采纳同一条提醒而不是重复注入。

`enter`、普通执行准入、Stop、关闭和显式 resume 共用同一运行协调。恢复提醒与任务完成仍调用既有设计中的 `ThreadMq` 接口；状态变化由生命周期行为提交到 `SessionResources`，业务方不通过裸 setter 或 SQL 分别修改 MQ、历史与工作意图。

## 5. 工作意图与 MQ 的事务关系

### 5.1 运行准入和收尾

- 主会话或 async 执行放行前提交 `working`。若提交后、worker 启动前退出，下次进入仍可继续原 thread。
- 准入前的 `working` 写入是 fail-closed：目标 thread 不存在或写入失败时拒绝本轮，不取 AgentPool、不调用模型；continuation 路径回滚本函数刚设置的 `continuation_in_flight` 与取消令牌。
- 自然收尾不能无条件写 `idle`。它与消息接纳共用协调，并在存储操作中确认没有仍要求继续处理的新工作，避免覆盖刚到的消息。收尾发生在 turn 之后，失败只能有限重试并记录 error，不能把已提交正文改回失败。
- 明确失败按现有失败规则停止本次自动尝试；后来的新输入仍按既有准入规则判断。
- 恢复准备失败不伪造任务完成或已取消；显示错误，保留已提交事实供下一次明确进入／继续重试，不建立无限后台恢复循环。残留提醒不再等价于「已注入」：进程内 admitted 状态负责防重复，MQ 中未消费的 entry 提醒可被下一次进入采纳。

### 5.2 消息接纳

需要推动目标继续工作的消息，按现有 typed policy 和执行绑定判定，在 MQ 接纳事务内维护目标工作意图：允许时从 `idle` 转为 `working`；`stopped` 不被被动回调解除；Info／UI-only／Passive 不自行授予执行资格。

该判定同时遵守第 2.1 节：本次同步执行不因为收到消息就被改成可自动恢复的 async 执行。收消息与实际后台交接是不同的生命周期操作。

这只记录工作意图。本进程尚未进入的根范围仍不能启动执行。已经激活的范围沿现有调度继续。

普通用户待发输入保留 Thread MQ 的忙碌、单发、全发、空闲队首和 Stop 规则；不能因为本设计统一写入工作意图，就排空所有待发输入。

### 5.3 本地 async Agent 完成

本地 child 的终态、`work_state` 收尾、结果接纳到目标 MQ，以及适用的接收方工作意图变更，使用同一 Store 原子操作。提交成功前不能宣称完成投递。

因此，已完成并成功入 MQ 的 child 不会因崩溃残留 `working` 而再次续跑。任务结果的稳定消息身份继续遵守 MQ 的去重契约；运行期任务登记本身不另建持久绑定表。

同一 child 的终态只写一次：重复结算（含 panic 兜底）不得改写已写下的终态，也不得因正文不同产生身份冲突。

具备跨行为事务的后端（本机 SQLite）全有或全无；远端与嵌入后端没有跨行为事务，只保证失败前不宣称完成，进程在两步之间退出可能留下已写终态。

### 5.4 Stop

Stop 按既有取消策略确定实际受影响对象，在持久操作中保存相应停止意图，再确认控制操作成功。不能只保存父级停止却让应受取消影响的 child 保留自动续跑资格。

恢复准备与 Stop 竞争时，旧快照不能覆盖新的 `stopped`。已经进入提交阶段的消息与当前执行按既有协调结算；Stop 不回删已提交历史，待发消息继续留在 MQ。

## 6. 外围任务边界

- Peri 不恢复 MCP 服务、进程、句柄或旧连接；只有外围服务仍持有的任务能被查询发现。
- Workspace 需要提供按直接 owner 查询当前存活 Shell 的能力，复用现有 MCP 能力面。不会扫描并接管其他 thread 的任务。
- 当前查询结果进入任务投影；查询失败表达为未知／失败，不能伪装成空列表。影响该 Agent 恢复准备的失败不阻止无关会话正常读取。
- 对本次发现的任务建立订阅后重新核对当前状态，处理发现与监听之间的变化；重复通知沿稳定外围 task 身份收敛。
- 不补播 downtime 期间的完成、输出或告警事件。没有发现某个旧任务，不等于它成功、失败或已取消。
- 正常重连后的新完成结果走原 owner 的 MQ；不能直接写入历史，也不能解除用户 Stop。

## 7. 历史与 Compact 的边界

消息成功进入 `messages` 后，MQ 行即删除。冷恢复按照已提交的有效历史、flags 和 projection 继续构造上下文。

本版本不恢复上一次进程的逐条结果处理记录，不新增 `awaiting_response`，也不通过另一份 ID 集合或强制保护全部历史结果实现替代账本。当前进程运行期间原有结果保护仍正常使用；冷恢复不承诺精确还原原 Ready／InFlight／Covered 集合，不将该缺失伪装成“全部已处理”。

恢复提醒提供中断与当前任务事实，模型从正常有效历史继续。历史中的缺失工具结果沿既有模型视图完整性规则处理，不制造成功结果、不重放未知副作用。

## 8. 实际字段仿真

以下 ID 为可重复 SQL 仿真值。生产继续使用现有 UUID 类型；`agent_status` 与 `cancel_policy` 均为既有字段。

| id | parent_thread_id | cancel_policy | agent_status | work_state | context_usage_tokens |
| --- | --- | --- | --- | --- | --- |
| root-a | NULL | cascade | active | working | 42100 |
| child-a1 | root-a | independent | active | working | 19800 |
| child-a11 | child-a1 | independent | active | working | 6400 |
| child-a2 | root-a | cascade | active | idle | 9500 |
| child-a3 | root-a | independent | cancelled | stopped | 12000 |
| root-b | NULL | cascade | active | working | 31000 |
| child-b1 | root-b | independent | active | working | NULL |

`child-a2` 表示中断前同步运行、没有自动续跑意图的 child。其遗留 Active 状态在进入后的中断结算中按既有语义收敛，不能直接报告 Done，也不能阻止后续显式 resume。

Peri 启动时不执行恢复查询。进入 `root-a` 后，第 3 节查询返回：

| id | parent_thread_id | work_state | context_usage_tokens |
| --- | --- | --- | --- |
| root-a | NULL | working | 42100 |
| child-a1 | root-a | working | 19800 |
| child-a11 | child-a1 | working | 6400 |

三者各自通过准入后恢复。`root-b` 与 `child-b1` 不受影响；`child-a2` 不自动续跑；`child-a3` 保持停止。

若 `root-a.work_state = 'idle'` 而两个 async child 仍为 `working`，候选只包含这两个 child；root 的收件地址可以装配，root 模型不因装配而启动。

## 9. 验收契约

以下是必须验证的行为，不表示运行时代码已经实现或测试通过。

| 场景 | 预期 |
| --- | --- |
| 多个根均存有 working | 进程启动零自动续跑；进入 A 只检查 A 范围。 |
| 重复进入／并发进入同一根 | 每个 thread 最多一个当前运行实例；不重复注入同一次恢复提醒。 |
| 主会话 idle、async child working | child 可恢复；不把父级 idle 强行改为正在执行。 |
| 嵌套 async Agent | 保留原 thread 与直接会话归属，结果不统一转投 root。 |
| sync 中断 | 不自动续跑、不伪报 Done；后续显式 resume 可用。 |
| sync 实际转为后台继续 | 交接提交后带 working 意图，进入时按 async 恢复。 |
| 恢复准备期间 Stop／关闭／手动 resume | 启动前复核生效；旧快照、旧事件和旧 worker 不覆盖新状态。 |
| MQ 已移交、模型推进前退出 | 历史存在且 MQ 不重复；working 让进入后从历史继续。 |
| child 完成提交边界退出 | 终态与父 MQ 全有或全无；不发生结果已接纳而 child 被再次恢复。 |
| 最后一条回复与新回调竞争 | 不丢新消息的工作意图；被动回调不解除 stopped。 |
| 进入前已有可唤醒消息入 MQ | 接纳及意图持久化但不冷启动；进入后按 working 正常推进。 |
| 进入后重建 Shell 视图 | 只看到直接 owner 的存活任务；查询失败不等于零任务。 |
| 外围查询与订阅之间变化 | 通过重查当前状态收敛，不补播整个 downtime。 |
| Context 初次展示与失效 | 已保存值立即显示；NULL 不显示为 0；失效值不用于精确预算判断。 |
| 旧库升级 | 只新增本文两列；旧 Active 不触发自动恢复，历史与 MQ 内容不被改写。 |
| 结果处理范围 | 不出现任务绑定表、历史处理标记或隐藏的 pending-result 持久副本。 |
| 恢复准备失败（装配成功） | load/resume 返回 Ok，`_meta.peri.recovery` 报 `failed`/`partial`，历史保留；不可执行分类另以 `-32017` 拒绝 prompt。 |
| 响应发送失败后再次进入 | 只丢弃 Prepared 启动资格，MQ 提醒保留；下一次进入采纳同一条提醒，不重复注入。 |
| 冷 child 恢复 | 响应前补齐 TaskManager 绑定与在线 endpoint；父级不启动模型。 |
| 准入工作意图写入失败 | prompt 拒绝、模型零调用、工作意图不留下 `working`。 |

所有业务写入经 Rust 行为接口和同一 Session Store。恢复入口负责装配与准入，普通执行负责正常状态变化；TUI、ACP 和回调不直接执行 SQL。

## 10. 实施顺序

| 顺序 | 交付内容 | 进入下一步的条件 |
| --- | --- | --- |
| 1 | 完成 Thread MQ 独立重构：统一入口、待发事实、原子移交与区域过滤。 | MQ 文档中的事务、路由和界面契约通过。 |
| 2 | 迁移 `threads` 两列，接入 context 展示与工作意图写入。 | 旧库迁移正确；主会话、async、sync、Stop 和后台交接不会写错意图。 |
| 3 | 在真正的会话进入入口接入根范围检查、运行协调、父子装配与恢复提醒。 | 重复进入、嵌套 child、idle 父级和恢复／Stop 竞争通过。 |
| 4 | Workspace 提供按 owner 发现存活 Shell，恢复当前订阅与任务投影。 | owner 隔离、查询失败和查询／订阅间变化通过；不补播 downtime。 |
| 5 | 整体验证异常退出与再次进入。 | 使用真实 SQLite 提交边界验证 MQ、历史、工作意图及本地完成事务，无重放和错误冷启动。 |

本文只制定设计与实施计划，不代表运行时代码已经修改。文档 SQL 可独立仿真，但不能替代以上运行时验收。

文档校验已使用 SQLite 执行上述两列迁移，写入第 8 节七行数据并重新打开数据库，验证根范围隔离、idle 父级、stopped child、旧数据默认值以及字段约束。该校验没有运行 Peri Agent，也没有验证运行时崩溃恢复。
