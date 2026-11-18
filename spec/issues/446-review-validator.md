# #446 校验对抗与执行记录

状态：本单验收记录；不构成运行时契约。基线 `cee019f34337e420178be0c2671c98f8c23c4083`。

## 第一轮：先反证方案

| 意见 | 裁决 | 落实与理由 |
| --- | --- | --- |
| 仅检查文件存在会放过重复 ID、空表、非法等级及无验收条目 | 采纳 | 校验表结构、唯一 ID、L1–L5/U、非空目录、L1–L4 映射，负例测试覆盖 |
| 用 Markdown 源字节长度会把 URL 算成可见文字 | 采纳 | 按显示标签与解码实体计字符；ID、等级和各列均计入 100 字 |
| 路径存在不能证明来源章节存在 | 采纳 | 本地来源必须存在；Markdown 标题与显式 HTML 锚点可定位，重复标题带数字后缀 |
| 无证据不该被文档门强迫填“通过” | 采纳 | 接受具体 `缺口：…`，脚本成功明确只证明结构与定位，不代表运行时验收 |
| L5 登记强制追加 E2E | 拒绝 | L5 无映射也通过；测试编排仍由外置测试仓库负责 |
| 只交校验命令，无持续执行入口 | 采纳 | 接入 CI changes job（纯文档也执行）及 pre-commit，开销仅 Python 标准库 |
| 新引入 Markdown 解析框架、全网抓取与网络链接稳定性门 | 拒绝 | 增加依赖及偶发失败；本次只做可复现本地路径/锚点扫描，明确支持语法范围 |

## 第一轮实测

- `python3 scripts/test-feature-catalog.py`：22 项通过；包括非法 ID 混入合法表、重复 ID、L6、超长、失效路径/锚点、外链权威、空证据、未映射核心、未知映射、L5 豁免和代码块排除。
- `python3 scripts/check-feature-catalog.py --root ../peri --scan-links`：基线 306 份 tracked Markdown 发现 3 处失效路径，0 处失效锚点。退出 1（如实报告存量）。
- 存量：`docs/reference/oxidizer-fetch-upstream-research.md` 引用已拆分 `transport/router.rs`；Thread MQ active spec 两处相对目录少一级。已交文档审查者修正。

## 校验边界

支持 fenced code、inline code 排除，内联/显式 reference links、ATX 标题和 HTML id/name。不是完整 CommonMark 渲染器，不证明外部 URL 可访问，不证明功能集合已穷尽或证据满足行为语义；这些由独立人工源码审查负责。不能从测试文件存在推断测试在本基线通过。

## 第二轮

已对修改后的权威清单、根指引、Durability 与 async-only 条款交叉审查，并由主审反证校验器。

| 意见 | 裁决与结果 |
| --- | --- |
| 根缩减不得丢失 1000 Agent、Peri Loom 与 Community Swarms 已批准目标 | 采纳；回补为目标/规划，明确不等于吞吐或部署验收 |
| MQ 消息可按 policy 维护工作意图，却写成“不改变历史记录” | 采纳；限定为不恢复历史 sync 执行身份，避免与 idle→working 矛盾 |
| 校验器空边界、目录/自引用作契约、验收契约不匹配可假绿 | 采纳；新增结构反例并拒绝，契约要求本地 Markdown 文件且目录/验收引用同源 |
| Markdown 标题中的 snake_case 不应被当作强调删去下划线 | 采纳；增加标题与代码 link label 反例 |
| 清单仅靠存在不能证明覆盖全量基线 | 采纳；新增 `--inventory`，按清单基线 SHA 的固定范围逐路径比对、重复与处置理由检查 |
| U 可缺评级理由，验收状态可为空 | 采纳 features 审查；U 必须有评级缺口，映射实现/验证状态不得为空，新增两组反例 |
| 基线清单被要求自动纳入未来所有新增文档 | 不采用；这是本次旧文档处置证据，不成为新的动态运行时/权威 owner 表；新文档由当前索引和功能门维护 |

实测最终结果（结构门，不代表行为验收）：

- `python3 scripts/test-feature-catalog.py`：22 tests OK。
- `python3 scripts/check-feature-catalog.py`：PASS，功能来源路径/锚点及验收映射符合结构约束。
- `python3 scripts/check-feature-catalog.py --inventory`：PASS，基线 140 份范围文档无遗漏。
- `python3 scripts/check-feature-catalog.py --scan-links`：当前 tracked + 未忽略新 Markdown 文件 0 处本地路径/锚点失效；基线 3 处路径已消除，新增 0。
- `wc -l -c CLAUDE.md`：56 行、3959 字节，符合根预算；后续仅路由增补需再核对。

未运行 Rust 运行时或外置 E2E；本次不修改运行时。完整 CommonMark、外网 URL 可用性、功能穷尽性与证据行为意义仍为人工审查边界。
