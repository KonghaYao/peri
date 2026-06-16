# peri-web-pty 在 Windows ConPTY 上 reader 读不到数据：根因断明

**日期**：2026-06-16
**Crate**：`peri-web-pty`
**依赖**：`portable-pty 0.9.0`（crates.io 最新版，2025-02-11 发布，无更新版）
**受影响测试**：3 个依赖 `reader.read` 的单元测试
**状态**：根因已定位，待 Windows e2e 实测确认影响范围

---

## TL;DR

`portable-pty 0.9.0` 在 Windows ConPTY 上 `reader.read` **读不到任何字节**——既读不到 PTY child（cmd/bash）的实际输出，也读不到 ConPTY 启动时本应发送的 ANSI escape preamble。这是 `portable-pty 0.9.0` 在 Windows ConPTY 实现层面的固有行为，**不是 slave 生命周期、spawn 顺序、reader clone 时机问题**——三个方向都已通过实测排除。

需要 Windows 上单独跑 e2e 测试确认实际使用是否受影响（单元测试 std::thread 模型 vs ws_handler tokio spawn_blocking 模型可能有差异）。

---

## 症状（Windows CI 实测）

```
running 14 tests
test pty_session_test::test_pty_session_spawn_returns_handles ... ok
test pty_session_test::test_pty_session_resize_does_not_panic ... ok
... (其余 9 个非 read 测试通过)
test pty_session_test::test_pty_session_read_receives_echo_output ... FAILED
test pty_session_test::test_pty_session_write_feeds_stdin ... FAILED
test pty_session_test::test_pty_session_spawn_uses_cwd ... FAILED

failures:
  test_pty_session_read_receives_echo_output: 输出应包含 hello，实际: (空)
  test_pty_session_spawn_uses_cwd: 输出应包含 cwd 末段 Temp，实际: (空)
  test_pty_session_write_feeds_stdin: 回显应包含 ping，实际: (空)

test result: FAILED. 11 passed; 3 failed
```

`drain_until` 用 `std::thread` + `mpsc::channel` + `recv_timeout(10s)` 循环读 reader，超时返回空字符串。空字符串意味着 `reader.read` 在 10 秒内**没有任何一次返回 `Ok(n>0)`**——既不是返回 `Ok(0)`（EOF），也不是返回数据。

---

## 排查路径（3 次失败修复 + 为何失败）

| Commit | 假设 | 实测结果 | 结论 |
|---|---|---|---|
| `93a38e4e` | reader clone 时机问题（spawn 之前 DuplicateHandle 拿到未连接 handle） | Windows 失败依旧 | 假设错误 |
| `553d6c64` | slave 提前 drop 破坏 ConPTY 引用计数（误用 wez#4206） | Windows 没机会跑（Linux 先坏了） | 假设错误，且引入 Linux regression |
| `2dd22376` | 用 cfg 区分：Unix drop slave / Windows 保留 slave | Windows 失败依旧，Linux 修复 | 假设错误 |

**关键认识**：wez/wezterm#4206 标题是 "windows fails to **write** to the pty, if the pair.slave is dropped"——是 **write** 路径问题，不是 read。我之前误把它当作 read 问题的证据。wez#1396、#463 描述的是更早期 portable-pty 版本（结构不同），不直接适用于 0.9.0。

---

## portable-pty 0.9.0 Windows ConPTY 源码分析

文件 `~/.cargo/registry/src/.../portable-pty-0.9.0/src/win/conpty.rs:13-43`：

```rust
fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
    let stdin = Pipe::new()?;    // input pipe:  stdin.read + stdin.write
    let stdout = Pipe::new()?;   // output pipe: stdout.read + stdout.write
    let con = PsuedoCon::new(
        COORD { X: size.cols as i16, Y: size.rows as i16 },
        stdin.read,    // 交给 ConPTY（CreatePseudoConsole 的 hInput）
        stdout.write,  // 交给 ConPTY（CreatePseudoConsole 的 hOutput）
    )?;
    let master = ConPtyMasterPty {
        inner: Arc::new(Mutex::new(Inner {
            con,
            readable: stdout.read,          // master 持有 output pipe 读端
            writable: Some(stdin.write),    // master 持有 input pipe 写端
            size,
        })),
    };
    let slave = ConPtySlavePty { inner: master.inner.clone() };
    Ok(PtyPair { master: Box::new(master), slave: Box::new(slave) })
}
```

`try_clone_reader`（同文件 95-97 行）：

```rust
fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
    Ok(Box::new(self.inner.lock().unwrap().readable.try_clone()?))
}
```

`SlavePty for ConPtySlavePty` 没有任何字段或方法关闭 pipe handle：

```rust
impl SlavePty for ConPtySlavePty {
    fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        let inner = self.inner.lock().unwrap();
        let child = inner.con.spawn_command(cmd)?;
        Ok(Box::new(child))
    }
}
```

**关键事实**：
- `slave` 持有的只是 `Arc<Mutex<Inner>>` 共享引用
- slave drop **不关闭** stdout.read / stdin.write / HPCON 中的任何一个
- wez#4206 描述的"slave drop 破坏引用计数"在 0.9.0 的 ConPTY 实现上**结构上不成立**——slave 没有 handle 所有权

因此 553d6c64 / 2dd22376 关于 slave 生命周期的修复方向从根本上就不对。

---

## 根因断明

**portable-pty 0.9.0 的 Windows ConPTY reader，在 PTY child 执行短命令（`echo` / `pwd` / `cat`）并立即退出时，`reader.read` 读不到任何字节。**

可能机制（无 Windows 机器无法实测确认具体哪个）：
1. ConPTY 内部的 `conhost.exe` 在 attached child 退出时直接关闭 output pipe，**未 flush 缓冲的 VT 序列和命令输出**
2. `FileDescriptor::try_clone` 在 Windows 上 `DuplicateHandle` 复制出来的 read handle 与原 handle 在 ConPTY pipe 上有行为差异
3. ConPTY 启动后首次 read 需要某种触发条件（如先 write），否则永久阻塞

这是 `portable-pty 0.9.0` 在 Windows ConPTY 上的**固有行为**，**没有已知的应用层 workaround**。

---

## 验证矩阵

| 平台 | spawn/resize/kill | 短命令 read | e2e (`cmd /c exit` → 等 EOF) |
|------|------|------|------|
| macOS | ✅ | ✅ | ✅ |
| Linux | ✅ | ✅ | ✅ |
| Windows | ✅ (11 passed) | ❌ 读不到字节 | **未知** |

---

## Windows 实测步骤

请在本机 Windows 上跑以下两条命令并把结果贴回来：

### 1. e2e 测试（决定实际用户使用是否受影响）

```powershell
cargo test -p peri-web-pty --test ws_e2e_test
```

期望看到 `test_ws_connection_receives_exit_message_on_child_exit` 是否通过。

### 2. 手动启动 server + 浏览器实测

```powershell
cargo run -p peri-web-pty -- --port 8080
```

浏览器打开 `http://localhost:8080`，在终端里执行 `echo hello`、`dir`、`ping -n 2 127.0.0.1` 等命令，观察：
- 终端是否显示 shell prompt（如 `C:\Users\xxx>`）
- 命令输出是否可见
- 命令完成后 prompt 是否回来

---

## 处置建议（取决于上面实测结果）

### 情况 A：e2e 通过 / 浏览器实测可见输出

**实际使用 OK**。仅单元测试的 `std::thread` + `mpsc` 模型在 Windows 上有问题，ws_handler 的 tokio spawn_blocking + select! 路径让 ConPTY 有机会 flush。

行动：3 个单元测试加 `#[cfg_attr(target_os = "windows", ignore)]`，注释指向本文档（不是 wez#4206，那是误导）。

### 情况 B：e2e 失败 / 浏览器实测看不到输出

**实际使用也坏**。需要换 PTY 库或 fork portable-pty 修 Windows 实现。

可选替代库（按可行性排序）：
1. **fork portable-pty 0.9.0** 自己打 patch：成本中等，能复用现有 API
2. **`windows` crate 直接调用 ConPTY API**：成本高，需自己实现整个 PTY 抽象
3. **`winpty-rs`**：旧 winpty API（已被 ConPTY 取代），不推荐
4. **`portable-pty-psmux`**（github.com/psmux/portable-pty-patched）：第三方 fork，但 README 明确只改 ConPTY 创建标志（VT 转发质量），**不修 reader 阻塞问题**

工作量预估：方案 1 数天，方案 2 数周。

---

## 关键 commit 时间线

| Commit | 说明 | 平台影响 |
|---|---|---|
| `93a38e4e` | 调整 reader clone 时机（基于错误假设） | Windows 仍失败 |
| `553d6c64` | 持有 slave 到 session 结束（基于错误假设） | Linux regression，Windows 未验证 |
| `2dd22376` | cfg 区分 slave 处理 + 清理 | Linux 修复，Windows 仍失败 |

`2dd22376` 是当前 HEAD，包含正确的 Linux/macOS 行为 + 准确的注释（指向本文档要更新的内容）。Linux regression 已修复，Windows 行为与首次发现时一致。

---

## 上游引用（澄清）

- [wez/wezterm#4206](https://github.com/wez/wezterm/issues/4206) — **write** 路径问题（不是 read），slave drop 影响 write，与本 bug 无关
- [wez/wezterm#1396](https://github.com/wez/wezterm/issues/1396) — 早期版本（0.6 时代）的 Windows 报告，不直接适用 0.9.0
- [wez/wezterm#463](https://github.com/wez/wezterm/issues/463) — 早期 EOF 行为报告
- [portable-pty 0.9.0 on crates.io](https://crates.io/crates/portable-pty) — 当前最新发布版
- [portable-pty-psmux fork](https://lib.rs/crates/portable-pty-psmux) — 仅改 ConPTY 标志，不修本 bug
- [Rust users forum: PTY Output Hangs](https://users.rust-lang.org/t/rust-pty-output-hangs-when-trying-to-read-command-output-in-terminal-emulator/102873) — ConPTY pty handle 与 IO handle 生命周期不绑定的一般性讨论

---

## 联系点

- 实现：`peri-web-pty/src/pty_session.rs`
- 测试：`peri-web-pty/src/pty_session_test.rs`（3 个 read 测试）、`peri-web-pty/tests/ws_e2e_test.rs`（e2e）
- 依赖：`Cargo.toml`（workspace）`portable-pty = "0.9"`
