# Peri Agent 如何让长任务跑几个小时不中断——双阶段上下文压缩机制

> **[Peri Code](https://github.com/think-bbs/peri)** — 用 Rust 写的开源 Coding Agent，兼容 Claude Code 生态。<https://github.com/KonghaYao/peri>

我们有一次让 Peri 做消息管线重构，涉及 20 多个文件，跑了 3 小时。中间触发了 2 次 Full compact、4 次 Micro-compact，任务完成，没有中断。

没有 compact 的话，这个任务跑到一半就会因为上下文窗口超过限制而直接失败。更糟的是，很多场景下 Agent 不会报错——它会在接近窗口上限时丢弃前面的指令，输出质量急剧下降。

Peri 的 compact 机制让 Agent 能在长任务中持续运行，核心思路是把双阶段压缩嵌入推理-执行循环的每一次迭代中。这套机制的设计围绕三个问题展开——阶段划分、各阶段职责、触发时机。

## 双阶段压缩：Micro 先截断消息，Full 后精简提示词

Peri 的 compact 分两阶段执行。**Micro-compact** 发生在每一轮对话开始前，只处理消息列表——把最早的消息按规则截断或排除。边界计算基于 **ContextBudget**（上下文预算管理器，从 provider 回传的 token 统计中评估实际用量），默认 70% 触发。**Full compact** 发生在 Micro 不够用时——当微压缩后预算仍然超标，Full 接管并对系统提示词做精简，默认 85% 触发。

双阶段不是一开始就有的设计。早期版本只有 Full compact，每次 compact 都要动系统提示词，破坏了 **prompt cache**（Anthropic 的前缀缓存机制——缓存相同前缀来跳过重复计算，大幅降低 token 成本）。改成 Micro 先上后，大部分情况下 Micro 就够了——系统提示词不动，cache 完整保留，compact 几乎没有额外延迟。

## 上下文预算管理器在每轮对话前评估 Token 用量

ContextBudget 是 compact 的决策核心。它不自己计算 token 数，而是从 provider 的回传信息中读取实际用量。每次 LLM 调用返回时，provider 会带上本次请求的 token 统计，ContextBudget 累计这些数据并维护一个滑动窗口。

评估时机是每轮推理-执行循环的发送消息前阶段——在向模型发送新消息之前。这一设计解决了事后才发现超出限制的问题——compact 是预防性压缩，不是抢救性裁剪。等窗口满了再裁，模型已经丢失了最近的上下文。提前在 70% 触发，留出 30% 缓冲空间继续正常交互。

不同模型的上下文窗口差异很大——Claude Sonnet 是 200K，GPT-4 是 128K，DeepSeek V3 是 64K。用百分比阈值自动适配所有模型，不需要为每个模型单独配置。

## 消息截断按角色分级，确保工具调用和工具结果成对保留

Micro-compact 不是简单地从头删到够。消息按重要性分级处理——System 消息（模型的行为指令）永远保留，不动它，这保证了 prompt cache 不破。User 和 Assistant 消息从最早的开始截断，但有一条硬规则：工具调用指令和它对应的工具执行结果必须成对删除。单独留一条工具结果或工具调用，模型会收到一个无法匹配的工具交互，可能产生幻觉。

具体实现里，消息被分成可排除和必须保留两类。可排除的消息带着一个最小保留行数的标记，方便恢复时知道每条消息的原始长度。Compact 的实际执行是把标记了 truncated（截断显示）或 excluded（完全排除）的消息在传给模型前做物理截断或移除，而不是修改存储中的原始消息——这一点很重要，因为 compact 效果只影响当前 turn，下一轮还是从完整历史开始评估。

## Full compact 精简静态系统提示词，不动动态部分

Full compact 是最后手段。当 Micro 已经截断到只剩最近的消息、但预算仍然超标时，说明系统提示词本身占了太多空间。

Peri 的系统提示词分为两部分：**Frozen**（静态段，如工具描述、行为规则）和 Dynamic（动态段，如当前日期、skill 摘要）。静态段占了 prompt 的大头且结构不可变——这是刻意设计的，为了让 Anthropic 的前缀缓存命中率最高。Full compact 在静态段做精简——砍掉低优先级的工具描述、压缩规则说明——但不碰动态部分，因为那些是每轮动态变化的，体积不足以成为 compact 对象。

静态段的修改会破坏整个 prompt cache，所以 Full compact 只在 Micro 彻底不够用时才触发。触发后一整个 turn 的 LLM 调用都会遭遇缓存失效惩罚（cache miss，每次请求需要重新计算前缀，token 成本剧增）。这也是为什么 85% 阈值比 Micro 的 70% 宽松——触发代价太高，宁可让 Micro 多干点活。

## Compact 效果只影响当前轮次，不污染历史存储

compact 后的消息状态通过 **compact flags**（truncated / excluded 标记）持久化在消息结构上。关键设计是这些标记和消息内容分开存储——内容不变，只是带了个本轮展示时裁掉的注释。下一轮 compact 评估时，消息内容还是完整的，compact 重新计算裁多少。

早期有一个分支设计把 compact 后的结果直接写回消息存储，结果导致下轮评估时消息已经是裁过的，compact 再裁一轮——越裁越短，很快就没有上下文了。改成分离存储后，每轮 compact 都是从完整历史重新评估，不会累积误差。

回到那个 3 小时的消息管线重构任务——正是因为每轮对话前 ContextBudget 都在 70% 阈值主动触发压缩，Agent 才没有在任何一次 LLM 调用中被窗口上限拦住。Compact 让 Agent 工作在有效注意力区间里，而不是在容量饱和的边缘运行。

项目地址：[github.com/konghayao/peri](https://github.com/konghayao/peri)
