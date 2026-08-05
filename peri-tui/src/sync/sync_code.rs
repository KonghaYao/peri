//! 8 字符 Crockford Base32 同步码（r2-encrypted-transfer v1）。
//!
//! 已冻结语义：
//! - sender 每个 30 秒 epoch 注册一个新码；码从注册时刻起有效 60 秒；
//! - 显示格式 `XXXX-XXXX`；接受大小写，`O→0`、`I/L→1`，忽略连字符，拒绝 `U`；
//! - 码只定位 channel，**绝不**作为密码、能力令牌或数据密钥。
//!
//! 码值是 40-bit CSPRNG 随机数；服务端（Slice 2）执行同一归一化，并按注册
//! 时刻判定 60 秒有效期，不使用客户端 epoch 做时钟判定。

use ring::rand::{SecureRandom, SystemRandom};

/// Crockford Base32 字母表（排除 I、L、O、U）。
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 码字符数（8 字符 = 40 bit）。
pub const CODE_CHARS: usize = 8;

/// epoch 长度（秒）；sender 每 epoch 刷新一次码。
pub const EPOCH_SECS: u64 = 30;

/// 码自注册时刻起有效时长（秒）。
pub const CODE_VALID_SECS: u64 = 60;

/// 40-bit 码值上限（`2^40`）。
pub const MAX_CODE_VALUE: u64 = 1 << 40;

/// Crockford 字符 → 5-bit 值。
///
/// `O/o → 0`，`I/i/L/l → 1`，大小写均可；`U` 与任何其它字符返回 `None`。
/// 值 = 字符在 [`ALPHABET`] 中的位置（字母表非连续，不能按 ASCII 差值计算）。
fn crockford_value(c: char) -> Option<u8> {
    match c {
        '0' | 'O' | 'o' => Some(0),
        '1' | 'I' | 'i' | 'L' | 'l' => Some(1),
        '2'..='9' | 'A'..='Z' | 'a'..='z' => {
            let upper = c.to_ascii_uppercase() as u8;
            ALPHABET.iter().position(|&a| a == upper).map(|i| i as u8)
        }
        _ => None,
    }
}

fn encode_40(value: u64) -> String {
    let mut out = String::with_capacity(CODE_CHARS);
    for shift in (0..40).step_by(5).rev() {
        let idx = ((value >> shift) & 0x1F) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

fn decode_40(normalized: &str) -> anyhow::Result<u64> {
    let mut value: u64 = 0;
    for c in normalized.chars() {
        let v = crockford_value(c)
            .ok_or_else(|| anyhow::anyhow!("invalid sync code character: {c}"))?;
        value = (value << 5) | u64::from(v);
    }
    Ok(value)
}

/// 40-bit 同步码值（仅作 locator）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyncCode(u64);

impl SyncCode {
    /// 从 40-bit 值构造；超出范围返回错误。
    pub fn from_value(value: u64) -> anyhow::Result<Self> {
        if value >= MAX_CODE_VALUE {
            anyhow::bail!("sync code value out of range: {value}");
        }
        Ok(Self(value))
    }

    /// 生成新的随机码（40-bit CSPRNG）。
    pub fn generate() -> anyhow::Result<Self> {
        let rng = SystemRandom::new();
        let mut bytes = [0u8; 5];
        rng.fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("OS RNG failure"))?;
        let value = u64::from_be_bytes([0, 0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]);
        Ok(Self(value))
    }

    /// 原始 40-bit 值。
    pub fn value(self) -> u64 {
        self.0
    }

    /// 显示格式 `XXXX-XXXX`。
    pub fn display(self) -> String {
        let s = encode_40(self.0);
        format!("{}-{}", &s[..4], &s[4..])
    }

    /// 归一化的规范形式（大写、无连字符、8 字符）。
    pub fn normalized(self) -> String {
        encode_40(self.0)
    }

    /// 归一化并解析用户输入；任何非法字符/长度即拒绝。
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        Ok(Self(decode_40(&normalize(input)?)?))
    }
}

/// 归一化用户输入：trim、去连字符、转大写，返回规范 8 字符串。
///
/// 除连字符外的任何非 Crockford 字符（含空格、`U`）一律拒绝。
pub fn normalize(input: &str) -> anyhow::Result<String> {
    let cleaned: String = input.trim().chars().filter(|c| *c != '-').collect();
    if cleaned.len() != CODE_CHARS {
        anyhow::bail!(
            "sync code must be {CODE_CHARS} characters, got {}",
            cleaned.len()
        );
    }
    let mut out = String::with_capacity(CODE_CHARS);
    for c in cleaned.chars() {
        match crockford_value(c) {
            // 输出规范字母表字符：O→0、I/L→1、小写→大写。
            Some(v) => out.push(ALPHABET[v as usize] as char),
            None => anyhow::bail!("invalid sync code character: {c}"),
        }
    }
    Ok(out)
}

/// 当前 30 秒 epoch（`unix_secs / 30`）。
pub fn epoch(unix_secs: u64) -> u64 {
    unix_secs / EPOCH_SECS
}
