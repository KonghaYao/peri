use anyhow::{Result, anyhow};

use crate::sync::{crypto, protocol::SyncPackage};

/// 加密分片后的数据块
#[derive(Debug, Clone)]
pub struct ChunkData {
    /// 分片序号，从 0 开始
    pub seq: u32,
    /// 加密后的分片密文
    pub data: Vec<u8>,
}

/// 打包后的完整数据
#[derive(Debug)]
pub struct PackedData {
    /// 所有加密分片
    pub chunks: Vec<ChunkData>,
    /// 加密数据总大小（字节），供计算传输进度
    pub encrypted_size: usize,
}

impl SyncPackage {
    /// MessagePack 序列化
    pub fn to_msgpack(&self) -> Result<Vec<u8>> {
        rmp_serde::to_vec(self).map_err(|e| anyhow!("msgpack 序列化失败: {}", e))
    }
}

/// 打包并加密 SyncPackage
///
/// 流程：MessagePack 序列化 → AES-256-GCM 加密 → 64KB 分片
pub fn pack(sync_pkg: &SyncPackage, pair_code: &str) -> Result<PackedData> {
    // Step 1: MessagePack 序列化
    let msgpack_bytes = sync_pkg.to_msgpack()?;
    tracing::debug!("序列化包大小: {} 字节", msgpack_bytes.len());

    // Step 2: 密钥派生
    let key = crypto::derive_key(pair_code);

    // Step 3: AES-256-GCM 加密
    let encrypted = crypto::encrypt(&msgpack_bytes, &key);
    let total_size = encrypted.len();
    tracing::info!("加密数据: {} 字节，开始分片", total_size);

    // Step 4: 按 CHUNK_SIZE 分片
    let chunks: Vec<ChunkData> = encrypted
        .chunks(crypto::CHUNK_SIZE)
        .enumerate()
        .map(|(i, chunk)| ChunkData {
            seq: i as u32,
            data: chunk.to_vec(),
        })
        .collect();

    tracing::info!("分为 {} 个分片", chunks.len());
    Ok(PackedData {
        chunks,
        encrypted_size: total_size,
    })
}

/// 计算数据的 SHA-256 校验和
///
/// 用于 transfer_complete 完整性验证
pub fn compute_checksum(data: &[u8]) -> String {
    use ring::digest::{SHA256, digest};
    let d = digest(&SHA256, data);
    d.as_ref().iter().map(|b| format!("{:02x}", b)).collect()
}
