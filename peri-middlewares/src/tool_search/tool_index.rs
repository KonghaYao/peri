//! TF-IDF 搜索索引 — 工具索引构建、混合搜索（TF-IDF + 关键词）、工具查找

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use parking_lot::RwLock;
use peri_agent::tools::BaseTool;

use super::keyword_search;

/// 搜索结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub score: f64,
}

/// TF-IDF 索引内部结构
struct TfIdfIndex {
    /// 每个工具的词向量（词 → TF×IDF 权重）
    doc_vectors: HashMap<String, HashMap<String, f64>>,
}

/// 对文本进行分词
///
/// CJK 字符逐字分割，ASCII 按空格/下划线/连字符分割，全部转小写
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_ascii() {
            if ch == ' ' || ch == '_' || ch == '-' {
                if !current.is_empty() {
                    tokens.push(current.to_lowercase());
                    current = String::new();
                }
            } else {
                current.push(ch);
            }
        } else {
            // CJK 字符：先 flush ASCII buffer，然后每个字符一个 token
            if !current.is_empty() {
                tokens.push(current.to_lowercase());
                current = String::new();
            }
            tokens.push(ch.to_lowercase().to_string());
        }
    }
    if !current.is_empty() {
        tokens.push(current.to_lowercase());
    }

    tokens
}

/// 构建 TF-IDF 索引
fn build_tfidf_index(tools: &[Arc<dyn BaseTool>]) -> TfIdfIndex {
    let total_docs = tools.len();
    if total_docs == 0 {
        return TfIdfIndex {
            doc_vectors: HashMap::new(),
        };
    }

    // 收集每个文档的词频
    let mut doc_term_freqs: HashMap<String, HashMap<String, f64>> = HashMap::new();
    let mut doc_freqs: HashMap<String, usize> = HashMap::new();

    for tool in tools {
        let name = tool.name();
        let desc = tool.description();

        // 加权分词：name 权重 3.0，description 权重 2.5
        let name_tokens = tokenize(name);
        let desc_tokens = tokenize(desc);

        let mut term_freqs: HashMap<String, f64> = HashMap::new();

        for token in &name_tokens {
            *term_freqs.entry(token.clone()).or_insert(0.0) += 3.0;
        }
        for token in &desc_tokens {
            *term_freqs.entry(token.clone()).or_insert(0.0) += 2.5;
        }

        // 统计文档频率
        let seen_terms: std::collections::HashSet<&String> = term_freqs.keys().collect();
        for term in seen_terms {
            *doc_freqs.entry(term.clone()).or_insert(0) += 1;
        }

        doc_term_freqs.insert(name.to_string(), term_freqs);
    }

    // 计算 TF-IDF 向量
    let mut doc_vectors: HashMap<String, HashMap<String, f64>> = HashMap::new();
    for (doc_name, term_freqs) in &doc_term_freqs {
        let mut vector = HashMap::new();
        for (term, tf) in term_freqs {
            let df = *doc_freqs.get(term).unwrap_or(&1) as f64;
            let idf = (total_docs as f64 / (df + 1.0)).ln();
            vector.insert(term.clone(), tf * idf);
        }
        doc_vectors.insert(doc_name.clone(), vector);
    }

    TfIdfIndex { doc_vectors }
}

/// 余弦相似度
fn cosine_similarity(vec1: &HashMap<String, f64>, vec2: &HashMap<String, f64>) -> f64 {
    if vec1.is_empty() || vec2.is_empty() {
        return 0.0;
    }

    let mut dot_product = 0.0;
    let mut norm1 = 0.0;
    let mut norm2 = 0.0;

    for (term, w1) in vec1 {
        norm1 += w1 * w1;
        if let Some(w2) = vec2.get(term) {
            dot_product += w1 * w2;
        }
    }
    for w2 in vec2.values() {
        norm2 += w2 * w2;
    }

    let denom = norm1.sqrt() * norm2.sqrt();
    if denom == 0.0 {
        return 0.0;
    }
    dot_product / denom
}

/// 工具搜索索引
///
/// 使用 TF-IDF + 关键词混合搜索（0.6/0.4 加权），
/// 支持并发读（RwLock）。
pub struct ToolSearchIndex {
    tools: RwLock<HashMap<String, Arc<dyn BaseTool>>>,
    tfidf_index: RwLock<TfIdfIndex>,
    /// 缓存首次生成的 deferred tools 提示词，后续不再重新生成
    cached_prompt: RwLock<Option<String>>,
    /// 内容版本号（每次 `build()` 全量重建递增）
    ///
    /// **[P2-2]** 用于检测"同 count 但不同 content"场景——例如 MCP 重连后
    /// 工具数量相同但内容变了（描述/schema 更新），仅靠 `total_count()` 比对
    /// 会漏掉这种情况。版本号保证全量重建必然递增，触发 cache 失效重建。
    ///
    /// 用 `AtomicU64` 而非 `RwLock<u64>`——u64 是 Copy，无需 RwLock 的互斥语义，
    /// 且原子操作避免了 read/write 双重加锁的死锁风险（同一线程在
    /// `*lock.write() = lock.read()...` 表达式中会自死锁）。
    content_version: AtomicU64,
    /// cached_prompt 生成时对应的 content_version
    ///
    /// cached_prompt 可能落后于实际内容（set_cached_prompt 后又有 build）。
    /// 中间件比对 `content_version()` 与 `cached_prompt_version()`，若不一致
    /// 则 cached_prompt 已 stale，需要重建。
    ///
    /// 用 `RwLock<Option<u64>>` 因为读取时需要返回 owned u64（Copy OK），
    /// 但 Option 不便用 atomic 表示；写少读多场景 RwLock 足够。
    cached_prompt_version: RwLock<Option<u64>>,
}

impl ToolSearchIndex {
    /// 构造空索引
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            tfidf_index: RwLock::new(TfIdfIndex {
                doc_vectors: HashMap::new(),
            }),
            cached_prompt: RwLock::new(None),
            content_version: AtomicU64::new(0),
            cached_prompt_version: RwLock::new(None),
        }
    }

    /// 使用 deferred tools 构建索引
    pub fn build(&self, deferred_tools: Vec<Arc<dyn BaseTool>>) {
        let mut tools_map = self.tools.write();
        let tfidf = build_tfidf_index(&deferred_tools);

        for tool in &deferred_tools {
            tools_map.insert(tool.name().to_string(), Arc::clone(tool));
        }

        // 将已有工具重新纳入索引
        *self.tfidf_index.write() = tfidf;

        // 内容版本递增——任何全量重建都视为内容变化
        // （即使工具数量相同，描述/schema 可能已变）
        // AtomicU64::fetch_add 原子地完成读-改-写，无双重加锁风险
        self.content_version.fetch_add(1, Ordering::SeqCst);
    }

    /// 混合搜索
    ///
    /// 查询语法：
    /// - `select:CronCreate,Snip` — 按精确名称查找，逗号分隔
    /// - `+slack message` — `+` 前缀词为必选，其余为可选关键词
    /// - `slack message` — 纯关键词搜索
    ///
    /// 评分：关键词分数 × 0.4 + TF-IDF 分数 × 0.6
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        // select: 前缀 — 按精确名称直接查找
        if let Some(names_str) = query.strip_prefix("select:") {
            let tools = self.tools.read();
            let names: Vec<&str> = names_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            return names
                .into_iter()
                .filter_map(|name| {
                    let tool = tools.get(name)?;
                    Some(SearchResult {
                        name: tool.name().to_string(),
                        description: tool.description().to_string(),
                        parameters: tool.parameters(),
                        score: 1.0,
                    })
                })
                .take(limit)
                .collect();
        }

        let (required, optional) = keyword_search::parse_query(query);
        let tools = self.tools.read();
        let tfidf = self.tfidf_index.read();

        // 构建查询向量
        let query_tokens = tokenize(query);
        let mut query_vector: HashMap<String, f64> = HashMap::new();
        for token in &query_tokens {
            *query_vector.entry(token.clone()).or_insert(0.0) += 1.0;
        }

        let mut results: Vec<SearchResult> = Vec::new();

        for (name, tool) in tools.iter() {
            let desc = tool.description();
            let params = tool.parameters();

            // 关键词分数
            let kw_score = keyword_search::keyword_score(name, desc, &required, &optional);

            // 必选词缺失时硬过滤：跳过该工具
            if !required.is_empty() && kw_score == 0.0 {
                continue;
            }

            // TF-IDF 分数
            let tfidf_score = if let Some(doc_vec) = tfidf.doc_vectors.get(name) {
                cosine_similarity(&query_vector, doc_vec)
            } else {
                0.0
            };

            // 混合分数
            let score = kw_score * 0.4 + tfidf_score * 0.6;

            results.push(SearchResult {
                name: name.clone(),
                description: desc.to_string(),
                parameters: params.clone(),
                score,
            });
        }

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }

    /// 按名称查找工具
    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn BaseTool>> {
        self.tools.read().get(name).cloned()
    }

    /// 返回所有工具的 (name, description) 列表
    pub fn list_names(&self) -> Vec<(String, String)> {
        self.tools
            .read()
            .iter()
            .map(|(name, tool)| (name.clone(), tool.description().to_string()))
            .collect()
    }

    /// 返回 Markdown 格式的延迟工具列表（按名称排序，保证跨进程稳定）
    pub fn format_deferred_list(&self) -> String {
        let tools = self.tools.read();
        if tools.is_empty() {
            return String::new();
        }

        let mut entries: Vec<_> = tools
            .iter()
            .filter(|(name, _)| !name.starts_with("mcp__"))
            .collect();
        entries.sort_by_key(|(name, _)| *name);

        if entries.is_empty() {
            return String::new();
        }

        let mut lines = String::from("## Deferred Tools\n\n");
        lines.push_str("The following tools are not in your direct tool list. Use `SearchExtraTools` to search for them, then `ExecuteExtraTool` to invoke.\n\n");
        for (name, tool) in entries {
            lines.push_str(&format!("- {}: {}\n", name, tool.description()));
            let params = tool.parameters();
            let props = params.get("properties");
            if let Some(props) = props.and_then(|p| p.as_object()) {
                if !props.is_empty() {
                    lines.push_str("  Parameters:\n");
                    for (param_name, param_schema) in props {
                        let desc = param_schema
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("");
                        let param_type = param_schema
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("any");
                        lines.push_str(&format!(
                            "    - `{}` ({}): {}\n",
                            param_name, param_type, desc
                        ));
                    }
                }
            }
        }
        lines
    }

    /// 返回索引中的工具总数
    pub fn total_count(&self) -> usize {
        self.tools.read().len()
    }

    /// 获取缓存的提示词
    pub fn cached_prompt(&self) -> Option<String> {
        self.cached_prompt.read().clone()
    }

    /// 缓存提示词（首次生成后调用），同时记录当前 content_version
    ///
    /// 后续可通过比对 `content_version()` 与 `cached_prompt_version()`
    /// 判断 cached_prompt 是否落后于实际内容（stale）。
    pub fn set_cached_prompt(&self, prompt: String) {
        let version = self.content_version.load(Ordering::SeqCst);
        *self.cached_prompt.write() = Some(prompt);
        *self.cached_prompt_version.write() = Some(version);
    }

    /// 返回当前内容版本号（每次 `build()` 递增）
    pub fn content_version(&self) -> u64 {
        self.content_version.load(Ordering::SeqCst)
    }

    /// 返回 cached_prompt 生成时对应的 content_version
    ///
    /// `None` 表示尚未生成过 cached_prompt；与 `content_version()` 不一致
    /// 表示 cached_prompt 已 stale，需要重建。
    pub fn cached_prompt_version(&self) -> Option<u64> {
        *self.cached_prompt_version.read()
    }
}

impl Default for ToolSearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "tool_index_test.rs"]
mod tests;
