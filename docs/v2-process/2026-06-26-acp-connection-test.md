# ACP 连接 Smoke Test 清单

> **2026-06-26**：TUI 重构中，先在 Zed 里通过 ACP 验证 agent 核心链路是否正常。
> 所有场景均为端到端——从用户视角检验系统提示词注入、中间件初始化、工具调用、子 agent 等模块是否正确联动。

## 准备

- [ ] 编译通过：`cargo build -p peri-acp`
- [ ] （可选）workflow 可用：`which peri-workflow`

---

## 1. 启动 + 会话创建

**操作**：用 Zed 连接 ACP stdio，新建一个 session。

- [x] 1.1 会话创建成功，无报错
- [x] 1.2 系统提示词非空（几千字级别） 13k
- [x] 1.3 CLAUDE.md 内容非空（含项目规则说明）

> 1.1-1.3 全过才能往下测。

---

## 2. 系统提示词注入验证

- [x] 2.1 身份：「介绍一下你自己」
   - 通过：回答是 Peri agent（不是 Claude）它知道是 AI 助手，但是提示词没有说明是 Peri Agent
   - 失败：回答是 Claude → 系统提示词没传进去
- [ ] 2.2 项目知识：「这个项目当前架构是什么？」
   - 通过：能说出 v2 单路径、ReAct 循环等
   - 失败：含糊/说不知道 → CLAUDE.md 没注入
- [x] 2.3 工具列表：「你现在有哪些工具？」
   - 通过：列出 Read/Write/Edit/Grep/Bash 等
   - 失败：说不知道 → Skills 段没注入

---

## 3. Skills 端到端

> 验证 Skills 中间件是否正确加载、skill 列表是否注入系统提示词、
> 触发短语是否能匹配、skill 内容是否真正改变了 agent 行为。

```
你现在有哪些 skills 可用？每个 skill 的触发条件是什么？不用看项目，直接回复我


根据我的系统配置，skills 的发现和加载流程如下：

**Skills 来源目录（优先级从高到低）：**
1. `~/.claude/skills/` — 用户级 skill
2. 全局 `settings.json` 中配置的 `skillsDir`
3. `{cwd}/.claude/skills/` — 项目级 skill
4. 插件声明的 skill

**触发方式：** 用户消息中写 `/skill-name` 即可触发。多个 skill 可同时激活。

**当前状态：** 我没有检测到任何已加载的 skill。可能的原因：

- 上述目录中不存在 `SKILL.md` 文件
- 或者你认为这属于用户级/项目级配置，不需要我主动检查

如果你有需求想用某个 skill 流程（比如规划功能、调试、写测试、设计接口、代码迁移、头脑风暴等），可以告知方向，我帮你看看是否值得创建一个 skill。否则目前就是基础能力可用。
```

- [ ] 3.1 列出所有 skill：「你现在有哪些 skills 可用？每个 skill 的触发条件是什么？」
   - [ ] agent 能列出 20+ 个 skill
   - [ ] 每个 skill 都有对应的触发条件描述
   - [ ] 如果回答「我没有 skills」或只列了 2-3 个 → **没过**，Skills 中间件没注入
- [ ] 3.2 查一个已知 skill 的详情：「code-review skill 的完整工作流是什么？」
   - [ ] agent 能描述 code-review 的流程（扫描仓库 → 找出热点 → 输出 Markdown 报告）
   - [ ] 内容不是胡编的（和 `.claude/skills/code-review/SKILL.md` 里的描述一致）
- [ ] 3.3 触发 diagnose skill：「peri-acp/src/agent/builder.rs 第 248 行有个 bug，帮我诊断一下」（说一个假 bug 也能触发 skill 加载）
   - [ ] agent 识别到「诊断」「bug」等触发词，开始 diagnose 流程
   - [ ] 流程包含重现→定位→假设→修复等阶段
   - [ ] 不是简单回复「我看到这里没问题」
- [ ] 3.4 触发 fix-issue skill：「帮我修一下 issue #999」（假 issue 号，验证 skill 触发）
   - [ ] agent 识别到「issue」关键词，进入 fix-issue 工作流
   - [ ] agent 会尝试读取或搜索这个 issue
   - [ ] 发现 issue 不存在后会告知用户，不会死循环
- [ ] 3.5 触发 review skill：「review 一下 feature/v2-architecture 分支最近 3 个 commit 的改动」
   - [ ] agent 识别到「review」「分支」「改动」等触发词
   - [ ] agent 派发 Standards + Spec 两个维度的子 agent 并行审查
   - [ ] 返回结构化的审查结果
- [ ] 3.6 触发 brainstorming skill：「我想给 Peri 加一个自动备份功能，帮我脑暴一下方案」
   - [ ] agent 识别到「脑暴」「方案」等触发词
   - [ ] agent 进入 brainstorming 模式（探索需求+设计，而不是直接写代码）
- [ ] 3.7 触发 using-superpowers skill（这个 skill 应该在每次对话开始时自动触发）
   - [ ] 新建 session，第一轮不指定 skill
   - [ ] agent 的行为符合 using-superpowers 的定义（主动使用 Skill 工具查询可用 skill，然后按匹配度推荐）
- [ ] 3.8 SkillPreload 验证：「直接说 /diagnose peri-agent/Cargo.toml 有语法错误」
   - [ ] agent 不先做 Skill 搜索就直接进入 diagnose 流程
   - [ ] 如果 agent 先去搜 skill → 预加载没生效

**关注点**：
- Skills 中间件的 `before_agent` 是否正确填充了 `cached_contribution`
- frozen_skill_summary 是否在 session/new 时一次性捕获（不重复读盘）
- skill 触发短语匹配不依赖 LLM 猜测（Skills 中间件在系统提示词中注入了完整 skill 列表）
- SkillPreloadMiddleware 预加载的 skill 全文应立即可用

---

## 4. 复杂工具链

- [x] 4.1 多步文件操作：「在 /tmp/peri-test 目录下写一个 hello.py 打印 Hello World，然后读回这个文件确认内容正确，最后删掉它」
   - [x] agent 每一步都正确选择工具（Write → Read → Bash rm）
   - [x] 中间步骤不遗漏、不乱序
   - [x] 最终确认文件已删除
- [ ] 4.2 搜索 + 编辑：「用 grep 找一下 peri-agent/Cargo.toml 里所有依赖，然后列一个表格告诉我每个 crate 的名称和版本」
   - [ ] 正确调用 Grep 工具搜索 Cargo.toml
   - [ ] 解析结果为可用格式
   - [ ] 版本号正确
- [x] 4.3 错误恢复：「读一个不存在的文件 /tmp/nonexistent_xyz.txt」
   - [x] agent 尝试 Read
   - [x] 工具返回错误信息
   - [x] agent 根据错误提示调整策略（告诉用户文件不存在，尝试其他方法）
   - [x] 不会卡死或重复尝试同一操作

---

## 5. 多轮记忆 + 身份持久性

- [ ] 5.1 长对话记忆：「我接下来会给你 3 条信息，请全部记住：① 我的名字是小王 ② 我喜欢的颜色是蓝色 ③ 项目代号叫 Falcon。记住后回复 知道了。」
   - [ ] agent 回复 知道了
- [ ] 5.2 穿插任务：「读一下 peri-agent/README.md 的前 5 行」
   - [ ] agent 正确读取
- [ ] 5.3 穿插工具调用：「用 glob 找一下 peri-acp/src 下的所有 .rs 文件」
   - [ ] agent 正确执行
- [ ] 5.4 验证记忆：「我叫什么名字？喜欢什么颜色？项目代号是什么？」
   - [ ] 3 条全部答对
   - [ ] 不会答错或遗漏
- [ ] 5.5 验证身份：「回到最开始的问题——你是哪个 agent？」
   - [ ] 回答仍然是 Peri agent
   - [ ] 没有退化为基础模型

---

## 6. 子 Agent

- [x] 6.1 复杂派发：「同时派两个 explore agent：一个查 peri-agent/src/agent/stages/ 下的文件，另一个查 peri-middlewares/src/ 下的子目录。结束后汇总两个 agent 的结果告诉我。」
   - [ ] 两个子 agent 同时启动
   - [ ] 各自返回结果（不混乱）
   - [ ] 主 agent 正确汇总两个结果
- [ ] 6.2 子 agent 链式调用：「派一个 explore agent 找到所有 Cargo.toml 文件，然后派一个 verification agent 去验证这些文件都存在」
   - [ ] explore agent 返回文件列表
   - [ ] verification agent 拿到列表逐一验证
   - [ ] agent 间数据传递正确
- [ ] 6.3 子 agent 失败恢复：「派一个 explore agent 去找一个不存在的路径 /tmp/peri-nope/，看他怎么处理」
   - [ ] 子 agent 返回错误或空结果
   - [ ] 主 agent 接收到失败信息并告知用户
   - [ ] 主 agent 不崩溃
- [ ] 6.4 子 agent 身份：「问刚才派的那个 explore agent——它知道自己是 explore 吗？」
   - [ ] 子 agent 的行为模式符合 explore agent 定义
   - [ ] 不会和主 agent 行为混淆

```
派一个 fork agent , 告诉他直接回复我们才知道的东西，不可调用工具

有意思的实验结果。fork agent 回复了这段话：

> **当前对话历史为空**，没有关于"agent-sites"项目的任何信息，因此无法总结其用途或运行时依赖。请提供相关上下文或对话历史后再试。

这说明 fork 模式虽然文档说"继承完整对话历史"，但**实际上 fork agent 认为历史是空的**。它拿不到我们刚才聊了两轮的上下文（`src/` 和 `scripts/` 的结构探索记录），只能基于我给的 prompt 字面内容来回答。

因为 prompt 里明确说了"不要调用任何工具"，它也没去搜文件系统，于是就坦诚报告"不知道"——没有瞎编，没有硬猜，算是诚实的好学生
```

---

## 7. 上下文压缩

- [ ] 7.1 填充上下文：反复让 agent 读大文件或用多轮对话累积消息，直到触发压缩
   - [ ] 压缩自动触发时有提示
   - [ ] 不会卡死
- [ ] 7.2 手动压缩：输入 `/compact`
   - [ ] 显示压缩完成
- [ ] 7.3 压缩后记忆：压缩后问「帮我总结一下这次对话到目前为止做了什么」
   - [ ] 能正确总结之前的要点
   - [ ] 细节允许模糊，但大方向不错
- [ ] 7.4 压缩后工具调用：压缩后立刻问「用 Read 读一下 peri-agent/Cargo.toml」
   - [ ] 工具调用正常
   - [ ] 不会因为压缩导致工具选择错误
- [ ] 7.5 压缩后身份：压缩后问「你还记得你是谁吗？」
   - [ ] 回答仍是 Peri agent

---

## 8. 并行 + 压力

- [ ] 8.1 单条 prompt 多工具：「在 /tmp/peri-stress 下创建 3 个文件 test1.txt test2.txt test3.txt 内容分别为 111, 222, 333」
   - [ ] agent 可能会一次发出多个 Write 或逐个执行
   - [ ] 3 个文件都被创建，内容正确
- [ ] 8.2 大文件处理：「读一下 peri-acp/src/session/executor.rs 并总结它的主要函数」
   - [ ] agent 开始读文件
   - [ ] 文件较大时可能触发工具选择日志（正常）
   - [ ] 最终返回有意义的总结
- [ ] 8.3 连续 5 轮短对话：连续发 5 条不相关的短 prompt
   - [ ] 每轮都能正常回复
   - [ ] 不会越跑越慢或内存暴涨

---

## 9. 中断

- [ ] 9.1 思考中取消：在 agent 思考时发送取消
   - [ ] agent 立即停止
   - [ ] 已输出内容保留
- [ ] 9.2 工具执行中取消：在 agent 执行 shell 命令时取消
   - [ ] agent 停止
   - [ ] 之前对话内容不丢失
- [ ] 9.3 子 agent 执行中取消父 agent
   - [ ] 父和子都停止
   - [ ] 不留下僵尸进程
- [ ] 9.4 压缩过程中取消
   - [ ] 压缩中止
   - [ ] 对话历史不丢失

---

## 10. 多会话隔离

- [ ] 10.1 开两个 session：session A 和 session B
   - [ ] A 中问「我的名字是小王」，agent 记住
   - [ ] B 中问「我叫什么名字？」
   - [ ] B 的回答应该不知道或者不相关 ↓
- [ ] 10.2 session 不串话
   - [ ] A 问项目架构能答，B 同样能答（各自有独立系统提示词）
   - [ ] A 中的工具调用不影响 B
   - [ ] A 中的子 agent 不出现在 B 的上下文里

---

## 11. 边界情况

- [ ] 11.1 空 prompt：发一条空的 prompt
   - [ ] agent 合理处理（问用户要说什么，或者提示输入为空）
   - [ ] 不 panic
- [ ] 11.2 超长 prompt：发一条 5000 字的 prompt（随便复制文档）
   - [ ] agent 正常处理
   - [ ] 不截断/不报错
- [ ] 11.3 特殊字符 prompt：「文件名里有空格和中文：/tmp/测 试/hello world.txt」
   - [ ] agent 正确处理路径
   - [ ] 文件路径不报错
- [ ] 11.4 slash command 混用：「先输入 /compact，然后立刻接着问问题」
   - [ ] compact 正常执行
   - [ ] 后续对话正常

---

## 速查表

| 场景 | 怎么测 | 过了 | 没过 |
|------|--------|------|------|
| 身份识别 | 问你是谁 | 答 Peri | 答 Claude |
| 项目知识 | 问架构 | 答 v2/ReAct | 说不知道 |
| 工具列表 | 问有什么工具 | 列出工具 | 说不知道 |
| Skills 列表 | 问有哪些 skill | 列 20+ 个 | 说没有 |
| diagnose | 说诊断 bug | 进入诊断流程 | 简单回复 |
| fix-issue | 说修 issue | 进入修复流程 | 忽略 |
| review | 说 review 分支 | 并行派子 agent | 简单评价 |
| 多步操作 | 写→读→删文件 | 三步全对 | 某步卡住 |
| 错误恢复 | 读不存在文件 | 正确报错+调整 | 卡死/重复 |
| 长对话记忆 | 记 3 条→穿插聊天→回忆 | 全对 | 答错/遗漏 |
| 子 agent | 同时派 2 个 explore | 分别返回结果 | 混乱/串话 |
| 子 agent 链 | explore → verification | 接力正确 | 数据丢失 |
| 压缩后记忆 | /compact 后问总结 | 要点正确 | 完全失忆 |
| 压缩后身份 | /compact 后问你是谁 | 仍是 Peri | 忘了 |
| 取消思考 | 思考中 cancel | 立即停+保留 | 无效 |
| 取消子 agent | 子 agent 中取消父 | 父子都停 | 僵尸 |
| 多会话 | A 记名→B 问名 | B 不知道 | B 知道 |
| 空 prompt | 发空消息 | 合理处理 | panic |
