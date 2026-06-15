# Peri Code: pty-server 如何从 side-project 成为 CLI 命令

> **[Peri Code](https://github.com/konghayao/peri)** — 用 Rust 写的开源 Coding Agent，兼容 Claude Code 生态。<https://github.com/KonghaYao/peri>

五月初我们在 Peri 仓库的 `side-projects/` 目录下放了一个叫 pty-server 的小东西。它是一个 Web 终端服务，前端用 xterm.js 渲染、后端通过 WebSocket 把浏览器里的按键事件转发给服务器上的 PTY（伪终端，一种模拟物理终端输入输出的软件接口）会话。最初的版本是 Bun 写的，一共三个文件——`server.ts` 108 行、`index.html` 81 行、`package.json` 8 行。当时的需求很简单：有个地方能在浏览器里跑终端就行，放在 side-projects 里合情合理。

接下来的五周里，这个 Bun 版本没再动过。直到今天下午，我们开始往上面加功能——多终端分屏、水平垂直切割、自适应网格布局，交互体验往 tmux 的方向靠。Bun 版本先做了一轮 JS 模块拆分，然后直接切到 Rust，用 portable-pty 做跨平台 PTY、axum 做 HTTP/WebSocket 服务。从第一个 Rust 提交到所有测试跑通，两个半小时。然后紧接着做了转正——改命名、提目录、挂 `peri web` 子命令。从 side-project 到一等公民，一个下午走完。

一个 side-project 该不该转正，标准不是代码量、star 数或用了多久，而是它是否到了用户应该用一条命令就能用上的阶段。pty-server 满足了这个条件——功能稳定、测试覆盖、日常在用、独立启动的体验不够好。转正是因为 pty-server 已经是一个正式功能了，只是还没挂上正式的名字和入口。

## Rust 重写后 pty-server 仍位于 side-projects 目录

用 Rust 重写有两个原因。第一，整个 Peri 技术栈是 Rust 的，pty-server 作为周边服务用 Bun 会引入额外的运行时依赖，用户装完 Peri 还得装 Bun 才能跑这个服务。第二，Bun 版本的 PTY 实现依赖 node-pty，跨平台行为不一致，macOS 上能跑、Windows 上需要额外配置。

用 portable-pty 重写之后，三个平台一个实现。axum 的 WebSocket 升级也是标准操作，`spawn_blocking` 读 PTY 输出、mpsc channel（多生产者单消费者通道，用于在线程间传递数据）推送到 WebSocket，逻辑清晰。重写完成后把它加进了 workspace member，`cargo build -p pty-server` 能编译，`cargo test -p pty-server` 全绿。

但它还是 side-project。目录在 `side-projects/` 下、包名叫 pty-server、启动方式是 `cargo run -p pty-server`。跟 Peri 主项目的唯一关联是共享了同一个 Cargo workspace——仅此而已。

## 转正需要三个改动

转正不是把目录挪个位置就完事。命名上，`pty-server` 改成 `peri-web-pty`，lib name 改成 `peri_web_pty`，跟 `peri-agent`、`peri-tui`、`peri-middlewares` 对齐，一看就知道是 Peri 生态的一部分。目录上，从 `side-projects/pty-server/` 移到仓库根目录 `peri-web-pty/`，跟其他顶层 crate 平级。side-projects 是实验区，顶层是正式成员。入口上，`peri web` 子命令直接挂在 `peri-tui` 的 CLI 下面，用户不需要知道 `peri-web-pty` 这个二进制，一条 `peri web` 就能启动整个 Web 终端服务。

## 端口由操作系统自动分配

独立运行时，pty-server 默认监听 3000 端口。用户需要手动打开浏览器、输入 `localhost:3000`。作为 side-project 这没问题，但作为 `peri web` 命令的一部分，用户不应该操心端口号。

转正后端口默认值从 3000 改成了 0——在 TCP 协议里 0 表示由操作系统分配一个可用端口。启动后通过 `TcpListener::local_addr()` 拿到实际端口，打印出来然后调用系统的默认浏览器打开。`open_browser` 函数按平台分发——macOS 调 `open`、Linux 调 `xdg-open`、Windows 调 `cmd /C start`，命令执行失败就静默跳过，不影响服务启动。用户输入 `peri web`，浏览器自动弹出一个终端页面。

## 打开就是 Peri

`peri web` 跟独立 pty-server 还有一个关键区别——默认行为。独立运行 pty-server 只是打开一个空白终端，等用户自己输入命令。但 `peri web` 的用户场景是明确的，他们想在浏览器里用 Peri。

所以 `Config::from_env()` 方法给 `initial_cmd` 设了一个默认值 `"peri"`。第一个 WebSocket 连接建立后，服务器会往 PTY 注入这个命令，等效于用户在终端里手动敲了 `peri` 然后回车。浏览器弹出终端窗口时，Peri 已经在里面等着了。如果用户想注入别的命令，环境变量 `CMD` 优先级更高，`PORT`、`CWD`、`SHELL` 也都可以通过环境变量覆盖，不需要参数，不需要配置文件。

## 二进制体积增加 300KB

peri-web-pty 依赖 axum 和 portable-pty，这两个库都不小。但加到 `peri-tui` 的依赖树之后，release 构建的 `peri` 二进制从 13,746,352 字节涨到 14,046,544 字节，多了不到 300KB，增幅约 2%。axum 和 portable-pty 的代码只会在 `peri web` 分支被调用，但因为 Rust 的静态链接特性，代码仍然会进入二进制。300KB 的增量对于一个 13MB 的二进制来说可以忽略。

项目地址：[github.com/konghayao/peri](https://github.com/konghayao/peri)
