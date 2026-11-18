# 真实复现方法（TUI 坐标类问题）

只写单测不足以证明用户可见行为。坐标、命中、选区类 bug 至少做一次真实终端对照，并记录修复前后的具体数字。

## 1. 从 worktree 构建

```bash
cd "$WT"   # 第 1 步选定的 worktree 绝对路径；已在其中时直接用当前目录
./scripts/cargo-rmcp-patched.sh build -p peri --locked
```

二进制在 worktree 的 `target/debug/peri`。不要用主 checkout 的旧二进制。

## 2. 启动隔离的真实终端

```bash
tmux new-session -d -s fixrepro -x 140 -y 40
tmux send-keys -t fixrepro "HOME=$PWD/.tmp/home ./target/debug/peri" Enter
```

- 用隔离的 `HOME`，避免真实会话、插件和历史干扰。
- 需要模型时用本地假模型或回放服务（仓库 `e2e/helpers/replay-model.ts` 可复用），不要让复现依赖真实 API。
- 宽终端至少 120 列；同时保留一个 <100 列会话验证 `left_pad = 0` 的回归。

## 3. 记录渲染坐标

1. 用 `tmux capture-pane -p -J -t fixrepro` 抓取可见帧。
2. 在测试文本中放唯一锚点（例如 `MOUSE-REPRO-ANCHOR`），记录它的 0-based 行列。
3. 计算 `GridSpec::grid_for(width)` 的 `left_pad` 与 `band_width`，记录按钮/文本的视觉列。

## 4. 注入真实鼠标事件

SGR 鼠标序列格式（坐标是 1-based）：

- 按下：`ESC [ < 0 ; COL ; ROW M`
- 拖动：`ESC [ < 32 ; COL ; ROW M`
- 释放：`ESC [ < 0 ; COL ; ROW m`

通过 pane 的 stdin 发送原始字节（例如 `tmux send-keys -l` 或 PTY 测试器；使用 `tmux send-keys -H` 时按十六进制发送）。每一步后用 `capture-pane` 核对状态栏反馈。

测试顺序：

1. 点击按钮视觉列 `C`，记录是否命中；再点 `C ± left_pad`，确认偏移方向与数值。
2. 在可见文本列 `A..B` 拖动，读取剪贴板或应用反馈，核对复制内容是否与光标范围一致。
3. 缩窄到 80 列重复一次，确认行为不变。
4. 修复后重新构建并重启再复验：`./scripts/cargo-rmcp-patched.sh build -p peri --locked`，退出旧进程并执行 `tmux kill-session -t fixrepro`，再按第 2 步分别以宽终端和 80 列新建会话启动 TUI。复用修复前的进程或旧会话会保留旧二进制/旧尺寸，结果无效。然后用同一组坐标重复测试，记录新的命中列与复制内容。

## 5. 记录模板

```text
终端：140×40，left_pad=11，band_width=117
修复前：按钮视觉列 99..102；点击 100 无反馈，点击 111 命中
修复前：拖动可见列 20..41，复制 "01: the quick brown "（整体左移 11）
修复后：按钮视觉列 110..113；点击 111 命中
修复后：拖动可见列 20..41，复制 "01: the quick brown f"
```

## 6. 清理

```bash
tmux kill-session -t fixrepro
rm -rf .tmp/home
```

不要把临时 HOME、抓屏文件或调试脚本提交进 PR。
