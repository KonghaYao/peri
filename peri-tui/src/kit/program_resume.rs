//! OSC 7577 程序恢复上报（Program Resume Protocol）。
//!
//! 终端重启后按程序上报的命令把会话恢复回来。终端把 token 注入 `OSC7577_TOKEN`，
//! 程序用 `sha256(body + ":" + token)` 给它签名，所以只是路过的输出（`cat`、日志、
//! ssh）无法注册命令。规范：
//! <https://www.superlogical.com/rex/docs/build/program-resume>。
//!
//! 只在 stdout 是 TTY 时发送；`PERI_PROGRAM_RESUME=0|off|false|no` 可关闭上报，
//! 未注入 token 时天然关闭。token 在 [`init`] 里从进程环境取走并删除，子进程不会
//! 继承，也不写进任何日志或序列以外的输出。序列以 ST（`ESC \`）结尾，与 OSC 7501
//! 保持一致。

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use parking_lot::Mutex;
use sha2::{Digest as _, Sha256};

/// 终端注入 token 的环境变量。取值不得写日志，也不得交给子进程。
const TOKEN_ENV: &str = "OSC7577_TOKEN";

/// 上报开关环境变量；`0|off|false|no` 关闭，未知取值按开启。
const ENABLE_ENV: &str = "PERI_PROGRAM_RESUME";

/// 协议限制：终端普遍接受的最大序列字节数与 argv 元素个数。
const MAX_SEQUENCE_BYTES: usize = 2048;
const MAX_ARGV_ELEMENTS: usize = 64;

/// 随恢复命令携带的显式 CLI 存储参数（原拼写白名单）。
const STORAGE_ARG_NAMES: [&str; 8] = [
    "--db-path",
    "--dbPath",
    "--session-store",
    "--sessionStore",
    "--session-store-engine",
    "--sessionStoreEngine",
    "--session-store-token-env",
    "--sessionStoreTokenEnv",
];

/// 恢复命令：程序与参数，外加可选的绝对工作目录。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumeCommand {
    argv: Vec<String>,
    cwd: Option<String>,
}

/// 上报身份：`time` 是 Unix 秒；终端会忽略与自身时钟差太远的报告。
/// 不派生 `Debug`：结构体持有 token，避免将来被顺手格式化进日志。
#[derive(Clone, Copy)]
struct Sender<'token> {
    token: &'token str,
    pid: u32,
    time: u64,
}

impl Sender<'_> {
    fn now(token: &str) -> Sender<'_> {
        Sender {
            token,
            pid: std::process::id(),
            time: unix_seconds(),
        }
    }
}

/// 进程级上报状态：token 与启动命令行里显式给出的存储参数只在这里保存。
struct ResumeState {
    token: String,
    storage_args: Vec<String>,
    /// 终端当前持有的命令；None 表示终端没有本进程的记录。
    reported: Option<ResumeCommand>,
}

/// 初始化后的上报状态；`None` 表示没有 token，上报与清除都是空操作。
type SharedState = Option<Mutex<ResumeState>>;

static STATE: OnceLock<SharedState> = OnceLock::new();

/// 退出是否由 SIGTERM 触发。终端正要消失，恢复命令必须保留。
static SIGNAL_EXIT: AtomicBool = AtomicBool::new(false);

/// 环境读取面（测试注入用）。
type Env<'env> = &'env dyn Fn(&str) -> Option<String>;

/// 取走终端注入的 token 并记录存储参数。必须在创建任何线程或子进程之前调用。
pub fn init() {
    let token = take_token();
    let storage_args = storage_args(std::env::args_os().skip(1));
    let state = token.filter(|token| !token.is_empty()).map(|token| {
        Mutex::new(ResumeState {
            token,
            storage_args,
            reported: None,
        })
    });
    let _ = STATE.set(state);
}

/// 上报当前会话的恢复命令；命令内容未变化时不重复发送。
pub fn report(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    let Some(state) = STATE.get().and_then(Option::as_ref) else {
        return;
    };
    let is_tty = std::io::stdout().is_terminal();
    let mut state = state.lock();
    let ResumeState {
        token,
        storage_args,
        reported,
    } = &mut *state;
    let command = resume_command(session_id, storage_args);
    let mut stdout = std::io::stdout().lock();
    let sender = Sender::now(token.as_str());
    if let Err(error) = emit_report(
        &mut stdout,
        reported,
        sender,
        command.as_ref(),
        &process_env,
        is_tty,
    ) {
        tracing::debug!(%error, "program resume: OSC 7577 report write failed");
    }
}

/// 正常退出前清除记录。SIGTERM 触发的退出保留记录：终端此刻正需要恢复命令。
pub fn clear() {
    if SIGNAL_EXIT.load(Ordering::Relaxed) {
        return;
    }
    let Some(state) = STATE.get().and_then(Option::as_ref) else {
        return;
    };
    let is_tty = std::io::stdout().is_terminal();
    let mut state = state.lock();
    let ResumeState {
        token, reported, ..
    } = &mut *state;
    let mut stdout = std::io::stdout().lock();
    let sender = Sender::now(token.as_str());
    if let Err(error) = emit_clear(&mut stdout, reported, sender, &process_env, is_tty) {
        tracing::debug!(%error, "program resume: OSC 7577 clear write failed");
    }
}

/// 标记本次退出由 SIGTERM 触发：终端正要消失，恢复命令必须保留。
pub fn mark_signal_exit() {
    SIGNAL_EXIT.store(true, Ordering::Relaxed);
}

/// 从进程环境取走 token 并从环境中删除，返回值只在内存里用于签名。
fn take_token() -> Option<String> {
    let token = std::env::var_os(TOKEN_ENV)?;
    // SAFETY: `init` 由 `main` 首行在创建任何线程或子进程之前调用（`main` 自身也
    // 还没有并发读取环境）；此后的子进程不会再继承 token。
    unsafe {
        std::env::remove_var(TOKEN_ENV);
    }
    token.into_string().ok()
}

/// 从启动命令行挑出显式给出的存储参数，保留原拼写与值。
///
/// 支持 `--opt value` 与 `--opt=value` 两种写法（后者规范化为两个参数）；
/// `--opt` 后紧跟 `-` 开头的参数视为缺值。白名单以外的参数（含 `--resume`）
/// 与环境变量值都不携带。
fn storage_args(args: impl IntoIterator<Item = OsString>) -> Vec<String> {
    let args: Vec<OsString> = args.into_iter().collect();
    let mut kept = Vec::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        let Some(text) = arg.to_str() else {
            continue;
        };
        if let Some((name, value)) = text.split_once('=') {
            if STORAGE_ARG_NAMES.contains(&name) {
                kept.push(name.to_owned());
                kept.push(value.to_owned());
            }
            continue;
        }
        if !STORAGE_ARG_NAMES.contains(&text) {
            continue;
        }
        let Some(value) = args.get(index).and_then(|value| value.to_str()) else {
            continue;
        };
        if value.starts_with('-') {
            continue;
        }
        kept.push(text.to_owned());
        kept.push(value.to_owned());
        index += 1;
    }
    kept
}

/// 组装恢复命令：`[current_exe, --resume, <session>] + 显式存储参数`。
fn resume_command(session_id: &str, storage_args: &[String]) -> Option<ResumeCommand> {
    let exe = match std::env::current_exe() {
        Ok(exe) if exe.is_absolute() => exe,
        Ok(exe) => {
            tracing::debug!(
                exe = %exe.display(),
                "program resume: current exe is not absolute; skipping report"
            );
            return None;
        }
        Err(error) => {
            tracing::debug!(%error, "program resume: cannot resolve current exe; skipping report");
            return None;
        }
    };
    let Some(exe) = exe.to_str() else {
        tracing::debug!("program resume: current exe is not valid UTF-8; skipping report");
        return None;
    };
    Some(ResumeCommand {
        argv: resume_argv(exe, session_id, storage_args),
        cwd: current_cwd(),
    })
}

/// 恢复命令以当前可执行文件开头，随后是 `--resume <session>` 与显式存储参数。
fn resume_argv(exe: &str, session_id: &str, storage_args: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(3 + storage_args.len());
    argv.push(exe.to_owned());
    argv.push("--resume".to_owned());
    argv.push(session_id.to_owned());
    argv.extend(storage_args.iter().cloned());
    argv
}

/// 发送时的绝对工作目录；拿不到或不是 UTF-8 时省略 cwd（仍上报 argv）。
fn current_cwd() -> Option<String> {
    let Ok(cwd) = std::env::current_dir() else {
        tracing::debug!("program resume: cannot resolve cwd; reporting without cwd");
        return None;
    };
    if !cwd.is_absolute() {
        tracing::debug!("program resume: cwd is not absolute; reporting without cwd");
        return None;
    }
    match cwd.to_str() {
        Some(cwd) => Some(cwd.to_owned()),
        None => {
            tracing::debug!("program resume: cwd is not valid UTF-8; reporting without cwd");
            None
        }
    }
}

/// 生成一条上报序列；协议无法承载时返回 None（调用方改发 clear）。
#[must_use]
fn format_report(sender: Sender<'_>, argv: &[String], cwd: Option<&str>) -> Option<String> {
    if !is_reportable(argv, cwd) {
        return None;
    }
    let encoded = argv
        .iter()
        .map(|arg| encode_text(arg))
        .collect::<Vec<_>>()
        .join(",");
    let mut pairs = format!("argv={encoded}");
    if let Some(cwd) = cwd {
        pairs.push_str(":cwd=");
        pairs.push_str(&encode_text(cwd));
    }
    let sequence = envelope(sender, &pairs)?;
    (sequence.len() <= MAX_SEQUENCE_BYTES).then_some(sequence)
}

/// 生成清除序列：`clear=1` 让终端删掉本进程的记录。
#[must_use]
fn format_clear(sender: Sender<'_>) -> Option<String> {
    envelope(sender, "clear=1")
}

/// 信封：`ESC ] 7577 ; <body> : mac=<hex> ESC \`。
fn envelope(sender: Sender<'_>, pairs: &str) -> Option<String> {
    if sender.token.is_empty() || sender.pid == 0 || sender.time == 0 {
        return None;
    }
    let body = format!("pid={}:time={}:{}", sender.pid, sender.time, pairs);
    let mac = digest_hex(&format!("{body}:{}", sender.token));
    Some(format!("\x1b]7577;{body}:mac={mac}\x1b\\"))
}

/// 规范要求的可上报条件；任何一条不满足都不发这条命令。
fn is_reportable(argv: &[String], cwd: Option<&str>) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    if argv.len() > MAX_ARGV_ELEMENTS {
        return false;
    }
    if !is_bare_name(program) && !is_absolute_path(program) {
        return false;
    }
    if let Some(cwd) = cwd
        && (!is_absolute_path(cwd) || !is_reportable_text(cwd))
    {
        return false;
    }
    argv.iter().all(|arg| is_reportable_text(arg))
}

/// argv[0] 的裸命令名形式：非空、不以 `-` 开头、不含路径分隔符。
fn is_bare_name(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-') && !value.contains(['/', '\\'])
}

/// 协议按 POSIX 路径定义；Windows 上再接受平台绝对路径（与 Go 实现一致）。
fn is_absolute_path(value: &str) -> bool {
    value.starts_with('/') || Path::new(value).is_absolute()
}

/// 控制字符会截断序列或变成终端按键；Rust 字符串天然是合法 UTF-8。
fn is_reportable_text(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

/// 标准 base64（带 padding）编码 UTF-8 字节。
fn encode_text(value: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
}

/// `sha256(input)` 的小写十六进制。
fn digest_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 发送上报：命令未变化时跳过；协议无法承载时若终端已持有旧命令则改发 clear。
fn emit_report(
    writer: &mut impl Write,
    reported: &mut Option<ResumeCommand>,
    sender: Sender<'_>,
    command: Option<&ResumeCommand>,
    env: Env<'_>,
    is_tty: bool,
) -> std::io::Result<()> {
    if !is_tty || !enabled(env) || command == reported.as_ref() {
        return Ok(());
    }
    let Some(sequence) =
        command.and_then(|command| format_report(sender, &command.argv, command.cwd.as_deref()))
    else {
        return emit_clear(writer, reported, sender, env, is_tty);
    };
    *reported = command.cloned();
    write_sequence(writer, &sequence)
}

/// 发送清除：只在终端已持有记录时发送。
fn emit_clear(
    writer: &mut impl Write,
    reported: &mut Option<ResumeCommand>,
    sender: Sender<'_>,
    env: Env<'_>,
    is_tty: bool,
) -> std::io::Result<()> {
    if !is_tty || !enabled(env) || reported.take().is_none() {
        return Ok(());
    }
    match format_clear(sender) {
        Some(sequence) => write_sequence(writer, &sequence),
        None => Ok(()),
    }
}

fn write_sequence(writer: &mut impl Write, sequence: &str) -> std::io::Result<()> {
    writer.write_all(sequence.as_bytes())?;
    writer.flush()
}

fn process_env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

/// `PERI_PROGRAM_RESUME` 显式关闭；未知取值按开启处理，与 `PERI_PROGRAM_STATUS` 一致。
#[must_use]
fn enabled(env: Env<'_>) -> bool {
    !matches!(
        env(ENABLE_ENV)
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("0" | "off" | "false" | "no")
    )
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
#[path = "program_resume_test.rs"]
mod tests;
