# Peri Code 的跨平台子进程封装——一个 `shell_command()` 统一三处手写平台判断

> **[Peri Code](https://github.com/konghayao/peri)** — 基于 [Claude Code Best](https://github.com/claude-code-best/claude-code) 改进的 Rust 开源 Coding Agent。<https://github.com/KonghaYao/peri>

Windows 用户配好 MCP 服务端，写的是 `"command": "npx"`。在 macOS 上一切正常，切到 Windows 后 Agent 直接报「找不到可执行文件」——`which npx` 明明能找到，但用异步运行时创建子进程传 `"npx"` 在 Windows 上走不通。

## npx 在 Windows 上启动不了

`npx` 在 Windows 上不是真正的可执行文件。它是一个 `.cmd` 批处理脚本，shell 认识它，但操作系统的 Process API 不认识。子进程创建方法底层调用的是操作系统的进程创建接口，这个接口不解析 `.cmd`——你得先启动 `cmd.exe`，让它去解析。

Windows 内核只认一种可执行格式：PE。进程创建接口拿着 `npx` 去 PATH 里找 `npx.exe`，找不到就报错。`.cmd` 不是 PE，内核不管。那命令行里敲 `npx` 怎么就能跑？答案是 `cmd.exe` 自己补了一刀——`PATHEXT` 环境变量里列了一串后缀（`.COM;.EXE;.BAT;.CMD`），`cmd.exe` 按这个表逐个拼接 `npx.com`、`npx.exe`、`npx.bat`、`npx.cmd`，找到哪个算哪个。这套匹配是 shell 做的，不是内核做的。你绕过 shell 直接调用进程创建接口传 `"npx"`，匹配不发生，崩。

macOS 和 Linux 不一样。内核的 `execve` 会读文件头——如果第一行是 `#!/usr/bin/env node`，内核自己去找 `node`，把脚本丢给它执行。直接在 macOS 上用子进程 API 启动 `npx` 能跑，就是因为它是个带 shebang 的 Node.js 脚本，内核代劳了解释器链。

但内核只管解释器，不管 shell。管道、重定向、`$VAR` 展开、`type` 这种内建命令——全都不在 shebang 的能力范围内。所以 Unix 上统一走 `bash -c`，不是画蛇添足——是让所有 spawn 的行为一致，不区分「shebang 能解决的」和「shebang 解决不了的」。

两套内核，两个 shell——Windows 上 `cmd /C npx`，非 Windows 上 `bash -c <command>`。一个函数收住。

这个问题不只影响 MCP。项目里任何需要通过 shell 启动子进程的场景都会踩到这个坑。Peri 有三个模块各自 spawn 子进程，但只有一处处理对了。

## 三个模块，三种写法

MCP 客户端在启动子进程传输的函数里直接拿用户配置的 `command` 字符串构造子进程命令，后面接 args 和 env，不做任何平台判断。配置里写 `npx`，Windows 上进程创建接口不认识——直接崩。

Bash 工具倒是自己做了一份平台切换——用条件编译判断当前平台，Windows 上起 `cmd /C`，非 Windows 上起 `bash -c`。能跑，但这段判断是 Bash 工具内部的私有逻辑，MCP 客户端和 Hook 执行器用不上。

Hook 执行器的做法又不同——它默认用 `bash`，构造子进程命令时指定 bash 可执行文件并传入 `-c` 参数。`shell` 的值可以配置，但默认就是 `bash`，Windows 上没有这个程序。即使想办法绕过去，Hook 执行器还要额外处理 Unix 侧 `bash -c` 的参数拼接——引用和转义。

三种写法，三种不同程度的失灵。MCP 客户端完全没处理，Hook 执行器处理错了，只有 Bash 工具是对的——但这套逻辑锁在 Bash 工具里，没人能复用它。

## 一个函数，两套 shell

正确的做法是把平台判断从三个模块里抽出来，做成一个独立的模块。新建 `peri-middlewares/src/process/mod.rs`，对外只暴露一个 `shell_command` 统一入口函数——它不做 spawn，只返回一个配置好的子进程命令对象。调用者拿到后自己配 env、cwd、stdio，和直接创建子进程命令一样的用法。

函数内部的逻辑是简单的平台路由——编译期用条件编译判断目标操作系统。Windows 分支构造 `cmd /C <command> <args...>`，命令和每个参数作为独立 argv 传给 `cmd.exe`。非 Windows 分支构造 `bash -c "<command> <args...>"`，因为 `bash -c` 只接收一个字符串作为脚本，所以需要把命令和参数拼成一行——如果参数里含空格、引号或反斜杠，用单引号包裹并转义内部单引号。

只是编译期二分，不做运行时检测。不自动识别 `.cmd` 后缀，不引入第三方异步运行时库，所有 spawn 场景一律走 shell 包裹。

## 三处调用改成一行

三条调用路径收敛成同一个函数——MCP 客户端把直接创建子进程的调用替换为统一的 shell 命令封装，用户配置里的 `command: "npx"` 不用改，Windows 上直接能跑。Bash 工具删掉那份手写的条件编译判断，换成同一个封装函数调用。Hook 执行器（`shell` 配置字段保留但被忽略，向后兼容已有的 Hook 配置）不再假设系统有 bash，全部交给这个封装函数。之后任何模块新增子进程 spawn，复用这个封装函数，不再手写平台判断。这条规则写进了 CLAUDE.md 的 [TRAP] 条目——所有子进程 spawn 必须通过统一的 shell 命令封装，新增 spawn 时必须复用。

回过头看，这个函数做的事很少——23 行代码，不做 spawn，不做超时，不做 stdout 管道。它只解决一个问题——不管什么平台，同一套调用，拿到一个能正确启动的子进程启动配置。下次在 Windows 上配好 MCP、Agent 说 `command: npx` 的时候，它能启动——不需要记得这套平台逻辑。

项目地址：[github.com/konghayao/peri](https://github.com/konghayao/peri)
