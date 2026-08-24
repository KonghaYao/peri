# 平台 CI 调试中以推测替代运行证据导致连续误修

**状态**：高优先级记录 / 待制度化
**优先级**：高
**类型**：工程方法 / CI 调试 / Agent 失败模式
**创建日期**：2026-08-24
**来源**：Windows PTC runtime、Ubuntu LSP 与跨平台 clippy 连续修复过程

## 问题

在 `fixture/ptc` PR 的跨平台 CI 调试中，Agent 多次在缺少目标平台运行证据时，将静态分析得到的候选原因直接表述为根因，并据此修改、提交和推送代码。部分修改解决了表层问题，但另一些修改无效或引入了额外迭代，显著增加了 CI 往返次数和用户校验成本。

最严重的问题不是单次判断错误，而是没有坚持“现象定位 → 阶段证据 → 最小对照实验 → 根因修复”的顺序。macOS 本地测试和 Windows cross-compile 被错误地赋予了 Windows runtime 验证能力；共享实现、失败行号和错误传播阶段也曾被误读。

## 失败模式

### 1. 将级联失败误认为已经解释

Windows 最初出现 `SpawnFailed("Access is denied. (os error 5)")`。移除 `CREATE_BREAKAWAY_FROM_JOB` 后解决了进程创建权限问题，但 Agent过早宣称错误分类、timeout、handshake 等失败会随之恢复。

事实是：启动权限只是第一层问题。修复后 Node 能启动，但仍在加载 artifact entry 时退出。一次修复只能解释它直接覆盖的证据范围，不能自动解释后续所有失败。

### 2. 在没有运行证据时猜测并行 Job Object 干扰

看到多个真实 Node 测试同时失败后，Agent推测 Windows Job Object cleanup 会终止其他并行测试，并引入 `serial_test`。Windows CI 串行后仍以相同方式失败，直接证伪该判断。

该改动的问题包括：

- 没有先证明终止 A 会影响 B；
- 没有先做两个 host 的隔离实验；
- 忽略了 Workflow 与 PTC 共享 `JsExecutionHost`、`RpcChannel` 和 `ProcessTree`；
- 使用测试调度规避尚未定位的生产启动失败。

### 3. 将 cross-compile 当作目标平台 runtime 验证

多次使用：

```bash
cargo check --target x86_64-pc-windows-msvc
```

作为 Windows 修复可信度的重要依据。该命令只能验证条件编译、类型和部分链接前构建面，不能验证：

- Windows `CreateProcess`；
- Job Object；
- pipe EOF/EPIPE 时序；
- Node CLI entry path；
- Windows 文件 handle 与 rename；
- GitHub runner 父 Job 环境。

后续还出现本机缺少 Windows SDK headers 导致 `aws-lc-sys` cross-check 中断。此类结果必须明确标为“编译边界检查”，不得称为 Windows 行为验证。

### 4. 错误读取失败阶段

Rust stdout reader 在 EOF 后使用内部 `RpcResponse(-32000)` drain pending request。Agent最初据此认定失败发生在 execute 后，实际这些测试在 `handshake()` 阶段就已失败。

只有在 handshake 和 execute 都接入 `RuntimeExited { success, code, stderr_bytes }` 后，CI 才给出直接证据：

- Node 统一以 code `1` 退出；
- stderr 字节数一致；
- 连预期执行 `process.exit(7)` 的手写 fixture 都未进入脚本正文。

这将失败边界收敛到“Node 加载 entry 之前”，最终定位为 Windows canonical/verbatim path 被直接作为 Node CLI 参数。

### 5. 使用错误的行号对比证明 CI 运行旧代码

Agent曾将 CI 的“断言失败行号”与当前源码的“测试函数定义行号”比较，错误宣布 CI 仍在运行旧 commit。实际当前断言就在 CI 报告的行号。

行号只能与同一语义位置比较，并且必须结合 commit SHA。不能仅凭行号判断 CI revision。

### 6. 未先验证 Workflow 所谓“正确经验”的覆盖范围

Agent一度把 Workflow 正常视为共享 Windows process/RPC 实现无误的强反证。后续检查发现 Workflow 的真实 process E2E 在 CI 中被 ignored；共享代码存在的 Windows Node entry path 问题并未被该测试覆盖。

正确做法是区分：

- 共享源码；
- 实际执行的测试；
- ignored 或 `cfg` 禁用的测试；
- 只覆盖协议纯函数的测试；
- 真正在目标平台启动进程的测试。

“模块已有测试”不等于“目标路径已被目标平台执行”。

### 7. 修复 `cfg` 行为时未同步编译面

PTC production-path E2E 使用 `#[cfg(not(windows))]` 暂停 Windows 运行，但测试专用 imports 未使用相同 cfg，最终 Windows clippy `-D warnings` 因 unused imports 失败。

禁用代码块时必须同步审查：

- imports；
- helper structs/impls；
- statics；
- dev-dependencies；
- clippy 的 `--all-targets` 编译面。

不能只确认 Unix tests 通过。

### 8. 提交和推送过早

在缺少 Windows 直接证据时，Agent连续创建并推送多个 commit。虽然用户允许在 PR 分支迭代，但 PR 可逆不代表可以降低证据标准。每次无效提交都会增加 review 噪声、CI 时间和后续回滚成本。

## 最终确认的技术根因

Windows `Path::canonicalize()` 返回 verbatim/extended-length path，常见形式为：

```text
\\?\C:\...
```

PTC artifact validator 将 canonical path 同时用于 package containment 安全校验和 Node CLI entry。Node 在该调用方式下未进入脚本正文便以 code `1` 退出，导致 handshake pending request 被 EOF drain。

最终修复将两个职责分离：

- canonical path：仅用于 symlink 解析、文件存在性与 package containment 校验；
- 普通 absolute path：用于 Node argv。

同类修复同步应用到 PTC 与 Workflow artifact validator。

## 后续强制证据门

处理平台专属 CI 失败时，按以下顺序执行；前一项没有证据，不得跳到后续根因声明。

1. **确认 revision**
   - 获取 CI 的 commit SHA；
   - 与远程分支 SHA 比较；
   - 行号仅作辅助，必须比较相同语义位置。

2. **确认失败阶段**
   - 明确失败发生在 spawn、artifact load、handshake、execute、cleanup 或 assertion；
   - 内部错误包装不得覆盖 child exit status；
   - 如果阶段不明，先加安全诊断，不改行为。

3. **区分验证能力**
   - 本机单测：验证本机 runtime；
   - target `cargo check/clippy`：只验证跨平台编译面；
   - 目标平台 CI：验证真实 OS 行为；
   - ignored/`cfg` disabled 测试：视为没有覆盖。

4. **先做共享实现对照**
   - 比较实际调用路径、环境、cwd、argv、artifact、测试启用状态；
   - “共享底层”只能降低部分候选优先级，不能替代覆盖证明。

5. **候选必须可证伪**
   - 每个候选写出支持证据、反证和最小对照实验；
   - 没有对照实验，不得把“可能”“高度吻合”升级为“根因”。

6. **一次只改变一个边界**
   - 诊断 commit 与行为修复分开；
   - 不同时修改环境、Job Object、adapter、错误分类和测试调度；
   - CI 结果必须能归因到单一变化。

7. **提交前检查条件编译闭包**
   - `cfg` 测试、imports、helpers、statics 和 dev-dependencies保持同一边界；
   - 运行目标平台 `--all-targets -D warnings`；无法运行时明确记录阻塞原因。

8. **PR 可逆不降低证据要求**
   - 用户明确要求快速推送诊断 commit 时可以推送；
   - 必须标明“诊断，不是修复”；
   - 不得宣称目标平台已修好，直到目标平台 CI 通过。

## 验收标准

- [ ] 后续平台专属问题的分析明确标注“事实 / 候选 / 反证 / 待验证”。
- [ ] cross-compile 输出不再被描述为 runtime 验证。
- [ ] 失败阶段不明时先补安全诊断，再改行为。
- [ ] CI revision 通过 SHA 确认，不再单独依赖行号。
- [ ] `cfg` 变更同步检查 imports、helpers 和 `--all-targets`。
- [ ] 连续三次平台修复中，不出现未经对照实验即提交的猜测性行为改动。
- [ ] 成熟规则再提炼到 `docs/standards/`；本 issue 保留事故叙事，不复制到根路由文件。

## 相关提交

- `8024cd23`：移除 Windows `CREATE_BREAKAWAY_FROM_JOB`，解决 spawn access denied。
- `45e135c7`：错误的 Windows 测试串行化方案。
- `66bcfe47`：撤销串行化并加入初步进程退出诊断。
- `11b37440`：对齐 Workflow 环境与 adapter 处理经验，但尚未触及 entry load 根因。
- `3a8d304e`：将 handshake EOF 转换为真实进程退出诊断。
- `7c1c0f3b`：分离 canonical containment path 与 Node CLI entry，修复 Windows 根因。
- `ff541c7a`：同步 Windows-disabled E2E 的条件 imports，修复 cross-platform clippy。

## 相关文件

- `peri-js-runtime/src/artifact.rs`
- `peri-js-runtime/src/executor.rs`
- `peri-js-runtime/src/host.rs`
- `peri-js-runtime/src/process_tree.rs`
- `peri-workflow/src/runner.rs`
- `peri-acp/src/host/executor_flow_test.rs`
- `.github/workflows/ci.yml`
- `spec/issues/2026-08-23-windows-ptc-production-e2e-disabled.md`
