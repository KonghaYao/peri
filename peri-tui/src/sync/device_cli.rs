//! 设备身份与信任 CLI（r2-encrypted-transfer v1）。
//!
//! `peri sync device init/show/add/list/remove`：
//! - init：生成 Ed25519 + X25519 密钥，写入 keystore（keyring 优先；显式
//!   `--keystore-path` 只创建加密文件；无 keyring 且无 TTY 时 fail closed），
//!   仅公钥写入 `sync-identity.json`；
//! - show：显示本地身份（device_id、公钥、fingerprint、邀请文本）；
//! - add：解析 `peri://device/<id>?ed=&x=&n=` 邀请，人工核对 fingerprint 后
//!   写入 `sync-trusted-peers.json`（公钥 only）；
//! - list/remove：已信任设备管理；untrust 后不得再选择该 peer。
//!
//! 路径：`~/.peri/sync-identity.json`、`~/.peri/sync-trusted-peers.json`。

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::sync::device::{DeviceId, DevicePublic, TrustedPeer, TrustedPeers};
use crate::sync::keystore::{
    FileStore, KEYRING_SERVICE, KeyMaterial, KeyringStore, KeystoreSource, SecretStore,
    default_keystore_path, keyring_available, resolve_source,
};
use crate::sync::limits;

/// `~/.peri/sync-identity.json` 与 `~/.peri/sync-trusted-peers.json`。
#[derive(Debug, Clone)]
pub struct DeviceCliPaths {
    pub identity: PathBuf,
    pub peers: PathBuf,
}

/// 默认路径（`~/.peri/` 下）。
pub fn default_paths() -> Result<DeviceCliPaths> {
    let home = dirs_next::home_dir().context("failed to determine home directory")?;
    Ok(DeviceCliPaths {
        identity: home.join(".peri").join("sync-identity.json"),
        peers: home.join(".peri").join("sync-trusted-peers.json"),
    })
}

/// `device` 子命令组（clap 结构定义在 lib 侧，main.rs 直接引用）。
#[derive(Debug, Subcommand)]
pub enum DeviceAction {
    /// 初始化本地设备身份（生成密钥并写入 keystore + identity.json）
    Init {
        /// 用户可见设备名（默认 peri-device）
        #[arg(long)]
        name: Option<String>,
    },
    /// 显示本地设备身份与邀请文本
    Show,
    /// 通过邀请文本添加已信任设备（人工核对 fingerprint 后确认）
    Add {
        /// 邀请文本（peri://device/...）
        invite: String,
    },
    /// 列出已信任设备
    List,
    /// 解除对某设备的信任
    Remove {
        /// 设备 ID（base64url）
        id: String,
    },
}

/// CLI 入口（读 stdin 密码/确认；核心逻辑在 `*_impl` 变体，便于测试注入）。
pub fn dispatch(action: DeviceAction, keystore_path: Option<&Path>) -> Result<()> {
    match action {
        DeviceAction::Init { name } => {
            let password = prompt_password("New keystore password: ")?;
            let confirm = prompt_password("Confirm keystore password: ")?;
            if password != confirm {
                anyhow::bail!("passwords do not match");
            }
            run_device_init(name.as_deref(), keystore_path, &password)
        }
        DeviceAction::Show => run_device_show(),
        DeviceAction::Add { invite } => {
            // H3 复审修复：先解析邀请并打印 device_id/fingerprint，再交互确认，
            // 确认后才写入 trusted_peers.json（确认前不落盘）。
            add_interactive(&invite, keystore_path, &mut std::io::stdin().lock())
        }
        DeviceAction::List => run_device_list(),
        DeviceAction::Remove { id } => run_device_remove(&id),
    }
}

/// 初始化设备身份。`password` 用于创建加密 keystore 文件（keyring 不可用时）。
pub fn run_device_init(
    name: Option<&str>,
    keystore_path: Option<&Path>,
    password: &str,
) -> Result<()> {
    let paths = default_paths()?;
    init_impl(name, keystore_path, password, &paths)
}

/// 初始化设备身份（可注入存储路径；测试用）。
pub fn init_impl(
    name: Option<&str>,
    keystore_path: Option<&Path>,
    password: &str,
    paths: &DeviceCliPaths,
) -> Result<()> {
    if paths.identity.exists() {
        anyhow::bail!(
            "device identity already exists at {}",
            paths.identity.display()
        );
    }
    let material = KeyMaterial::generate()?;
    let device_id = DeviceId::random()?;
    let name = name.unwrap_or("peri-device");
    limits::validate_device_name(name)?;
    let public = DevicePublic::from_keys(
        device_id,
        material.ed25519_public(),
        material.x25519_public(),
        name,
    )?;

    // keystore 创建决策（fail closed，无明文回退）：
    // - 显式路径 → 仅创建加密文件；
    // - keyring 可用 → OS keyring（account = device_id）；
    // - 无 keyring + 无 TTY → 拒绝（禁止自动初始化或明文回退）。
    match keystore_path {
        Some(path) => {
            FileStore::create(path, password, &material)?;
        }
        None if keyring_available(KEYRING_SERVICE) => {
            KeyringStore::create(KEYRING_SERVICE, &device_id.to_b64(), &material)?;
        }
        None => {
            if !std::io::stdin().is_terminal() {
                anyhow::bail!(
                    "no usable OS keyring and no TTY to create the encrypted keystore: \
                     refusing to fall back to plaintext"
                );
            }
            let path = default_keystore_path()?;
            FileStore::create(&path, password, &material)?;
        }
    }

    write_identity_atomic(&paths.identity, &public)?;
    println!("Device initialized:");
    println!("  ID: {}", public.device_id);
    println!("  Name: {}", public.name);
    println!("  Fingerprint: {}", public.fingerprint());
    println!("  Invite: {}", public.invite_uri());
    Ok(())
}

/// 显示本地设备身份（只读公钥，不打开 keystore）。
pub fn run_device_show() -> Result<()> {
    let paths = default_paths()?;
    show_impl(&paths)
}

/// 显示本地设备身份（可注入存储路径；测试用）。
pub fn show_impl(paths: &DeviceCliPaths) -> Result<()> {
    let identity = load_identity(paths)?;
    println!("Device ID: {}", identity.device_id);
    println!("Name: {}", identity.name);
    println!(
        "Ed25519 pub: {}",
        crate::sync::canonical::b64url_nopad(&identity.ed_pub)
    );
    println!(
        "X25519 pub: {}",
        crate::sync::canonical::b64url_nopad(&identity.x_pub)
    );
    println!("Fingerprint: {}", identity.fingerprint());
    println!("Invite: {}", identity.invite_uri());
    Ok(())
}

/// 交互式添加已信任设备（H3 复审修复）。
///
/// 顺序：解析邀请 → 打印 device_id/fingerprint → 从 `input` 读取确认 →
/// 确认后才写 trusted_peers.json。`input` 可注入（测试 mock stdin 顺序）。
pub fn add_interactive(
    invite: &str,
    keystore_path: Option<&Path>,
    input: &mut dyn BufRead,
) -> Result<()> {
    let _ = keystore_path; // add 只写公钥，不需要私钥
    let paths = default_paths()?;
    add_interactive_impl(invite, input, &paths)
}

/// 交互式添加已信任设备（可注入存储路径与 stdin；测试用）。
pub fn add_interactive_impl(
    invite: &str,
    input: &mut dyn BufRead,
    paths: &DeviceCliPaths,
) -> Result<()> {
    // 1. 先解析邀请（非法邀请在询问前即拒绝，不产生任何写入）。
    let device = DevicePublic::parse_invite_uri(invite)?;
    println!("Device: {}", device.name);
    println!("  ID: {}", device.device_id);
    println!("  Fingerprint: {}", device.fingerprint());
    // 2. 再交互确认（此时用户已看到完整身份信息）。
    let confirmed = confirm_from(input, "Trust this device? [y/N]: ")?;
    if !confirmed {
        println!("Cancelled");
        return Ok(());
    }
    // 3. 确认后才写入。
    let mut peers = load_peers(paths)?;
    if peers.contains(&device.device_id) {
        anyhow::bail!("device {} is already trusted", device.device_id);
    }
    peers.add(TrustedPeer::from_device(&device, now_secs()))?;
    peers.save(&paths.peers)?;
    println!("Device {} added to trusted peers", device.device_id);
    Ok(())
}

/// 通过邀请文本添加已信任设备（`confirmed` 由人工指纹核对决定；
/// 非交互变体，测试直接注入确认结果）。
pub fn run_device_add(invite: &str, keystore_path: Option<&Path>, confirmed: bool) -> Result<()> {
    let paths = default_paths()?;
    add_impl(invite, keystore_path, confirmed, &paths)
}

/// 通过邀请文本添加已信任设备（可注入存储路径；测试用）。
pub fn add_impl(
    invite: &str,
    keystore_path: Option<&Path>,
    confirmed: bool,
    paths: &DeviceCliPaths,
) -> Result<()> {
    let _ = keystore_path; // add 只写公钥，不需要私钥
    let device = DevicePublic::parse_invite_uri(invite)?;
    let mut peers = load_peers(paths)?;
    if peers.contains(&device.device_id) {
        anyhow::bail!("device {} is already trusted", device.device_id);
    }
    println!("Device: {}", device.name);
    println!("  ID: {}", device.device_id);
    println!("  Fingerprint: {}", device.fingerprint());
    if !confirmed {
        println!("Cancelled");
        return Ok(());
    }
    peers.add(TrustedPeer::from_device(&device, now_secs()))?;
    peers.save(&paths.peers)?;
    println!("Device {} added to trusted peers", device.device_id);
    Ok(())
}

/// 列出已信任设备。
pub fn run_device_list() -> Result<()> {
    let paths = default_paths()?;
    list_impl(&paths)
}

/// 列出已信任设备（可注入存储路径；测试用）。
pub fn list_impl(paths: &DeviceCliPaths) -> Result<()> {
    let peers = load_peers(paths)?;
    if peers.is_empty() {
        println!("No trusted devices");
        return Ok(());
    }
    for p in &peers.peers {
        println!("{}  {}  {}", p.device_id, p.name, p.fingerprint());
    }
    Ok(())
}

/// 解除信任；untrust 后该设备不可再作为 Send 目标。
pub fn run_device_remove(id: &str) -> Result<()> {
    let paths = default_paths()?;
    remove_impl(id, &paths)
}

/// 解除信任（可注入存储路径；测试用）。
pub fn remove_impl(id: &str, paths: &DeviceCliPaths) -> Result<()> {
    let device_id = DeviceId::from_b64(id)?;
    let mut peers = load_peers(paths)?;
    if !peers.remove(&device_id) {
        anyhow::bail!("device {device_id} is not trusted");
    }
    peers.save(&paths.peers)?;
    println!("Device {device_id} removed");
    Ok(())
}

/// 读取 identity.json（不存在/损坏即失败）。
pub fn load_identity(paths: &DeviceCliPaths) -> Result<DevicePublic> {
    let raw = std::fs::read_to_string(&paths.identity).with_context(|| {
        format!(
            "cannot read device identity {} — run `peri sync device init` first",
            paths.identity.display()
        )
    })?;
    serde_json::from_str(&raw).context("invalid device identity file")
}

/// 读取 trusted peers（文件不存在视为空）。
pub fn load_peers(paths: &DeviceCliPaths) -> Result<TrustedPeers> {
    TrustedPeers::load(&paths.peers)
}

/// 打开设备私钥存储（fail-closed 决策见 `keystore::resolve_source`）。
pub fn open_device_store(
    keystore_path: Option<&Path>,
    identity: &DevicePublic,
) -> Result<Box<dyn SecretStore>> {
    let has_tty = std::io::stdin().is_terminal();
    let source = resolve_source(keystore_path, keyring_available(KEYRING_SERVICE), has_tty)?;
    match source {
        KeystoreSource::Keyring => Ok(Box::new(KeyringStore::open(
            KEYRING_SERVICE,
            &identity.device_id.to_b64(),
        )?)),
        KeystoreSource::File(path) => {
            let password = prompt_password("Keystore password: ")?;
            open_file_store(&path, &password)
        }
    }
}

/// 打开加密 keystore 文件（可注入密码的变体）。
pub fn open_file_store(path: &Path, password: &str) -> Result<Box<dyn SecretStore>> {
    Ok(Box::new(FileStore::open(path, password)?))
}

/// 以 0600（unix）原子写 identity.json（临时文件 + rename）。
fn write_identity_atomic(path: &Path, identity: &DevicePublic) -> Result<()> {
    use std::io::Write as _;
    let raw = serde_json::to_string_pretty(identity)?;
    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
    };
    #[cfg(not(unix))]
    let opened = {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
    };
    let mut file = opened.with_context(|| format!("cannot write {}", tmp.display()))?;
    file.write_all(raw.as_bytes())
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

fn prompt_password(prompt: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("no TTY available to read keystore password");
    }
    print!("{prompt}");
    std::io::stdout().flush()?;
    rpassword::read_password().context("failed to read password")
}

/// 从可注入 reader 读取 y/n 确认（H3：交互式 add 用；测试 mock stdin）。
fn confirm_from(input: &mut dyn BufRead, prompt: &str) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let t = line.trim().to_lowercase();
    Ok(t == "y" || t == "yes")
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
