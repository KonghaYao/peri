# Micro archive 批量验证实验计划

状态：两轮实验、两轮独立验证及最终对抗审查完成；本目录是测量记录，不是架构权威。设计见 [Micro Compact](../design/micro-compact.md)。

1. 问题：固定一次 committed_tail、每批至多32×4096 bytes，能否减少验证归档的串行资源/远端批次，同时保持原文认证与原子发布？不修改75%/95%/45%阈值。
2. 假设：新路径的body API次数为 ceil(K/32)，固定尾界metadata API一次；旧循环body API次数K、metadata API每catalog一次。真实性、取消与单次原子提交不变。
3. 第一轮：500条约4KiB结果；第二轮：32条约500KiB结果。各轮使用同样的fixture、planner、SQLite flush/commit，旧算法仅在测试中重现逐catalog/逐chunk等待，新算法使用生产apply_micro_plan。串行运行，不能并行竞争测量。
4. 记录：实际catalog/chunk/bytes、planner/apply/total wall、进程CPU（各phase）、SQLite append/commit wall、受控bytes port wall、metadata/body API次数、最大在飞批数和字节数、commit次数。fixture构造和编译不计入路径时间。原始输出存到每轮报告，记录命令、环境、退出码和真实测试数。
5. 另以真实RemoteSessionData + SQLiteTransport测试证明一个32块验证批对应一次read_batch。端口fixture的调用数不冒充真实Turso网络测量；不注入或推算网络RTT，不承诺生产延迟。
6. 验证者独立审查每轮原始输出、计数、方法和推论。发现缺口后调整下一轮；最后对综合结论做对抗审查。保留局限，不以planner时间替代全路径时间。

结果见[综合结论](conclusion.md)、各轮实验/验证报告及[最终对抗审查](adversarial_review.md)。


方案修订：1000步阻断writer时超过生产默认1024条积压上限，首次测量失败，无有效性能数据；改为500步，使全部原文写入仍可从计时起点开始，不改变生产预算或公开内部API。第二轮32×500000 bytes（含消息壳仍小于默认16 MiB）保持。两策略使用相同生产预算；append gate在clock开始后放行，确保计时覆盖全部真实canonical追加I/O。
