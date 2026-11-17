//! 无框架依赖的 OpenUI Lang 解析、校验和求值。
//!
//! 语义移植自 OpenUI lang-core，MIT，commit 1e24d7f5；归属与许可见 NOTICE。
//! `parse` 接收程序文本；Markdown 中的 `gen-ui` 块由 `scan_blocks` 单独提取。
//! `Element::props` 保留参数顺序，动态值由独立的类型化内部树保存，不从 JSON 猜测 AST。
//!
//! ```
//! use std::sync::Arc;
//! use peri_genui::{evaluate_root, parse, scan_blocks, EvalContext, Library, StreamParser};
//!
//! # fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//! let library = Arc::new(Library::from_json_str(r#"{
//!     "root": "Text",
//!     "schema": {"properties": {"Text": {}}, "$defs": {
//!         "Text": {"properties": {"text": {"type": "string"}}, "required": ["text"]}
//!     }}
//! }"#)?);
//! let blocks = scan_blocks("```gen-ui\nroot = Text(\"你好\")\n```");
//! let parsed = parse(&blocks[0].source, &library);
//! let evaluated = evaluate_root(&parsed, &EvalContext::default());
//! assert!(parsed.errors.is_empty() && evaluated.errors.is_empty());
//! assert_eq!(evaluated.root.as_ref().map(|root| root.type_name.as_str()), Some("Text"));
//! let mut stream = StreamParser::new(library);
//! stream.push("root = Text(");
//! stream.push("\"你好\")");
//! assert_eq!(stream.finish().to_fixture_json(), parsed.to_fixture_json());
//! # Ok(())
//! # }
//! ```
//!
//! ## Rust 边界
//!
//! - `Library::from_json_str` 保留 schema 属性顺序；`from_value` 无法恢复 `Value` 已丢失的顺序。
//! - `evaluate_root` 返回根和结构化错误，不执行 action，也不执行 Query/Mutation。
//! - `StreamParser` 每次重解析完整缓冲区，保证完成态与批量解析一致，不保证上游中间快照一致。
//! - JSON Schema 仅实现上游校验子集；不提供序列化回语言、编辑、表单交互或宿主执行器。

#![forbid(unsafe_code)]

mod actions;
mod ast;
mod builtins;
mod evaluator;
mod expressions;
mod lexer;
mod library;
mod materialize;
mod memory;
mod parser;
mod prompt;
mod scanner;
mod statements;
mod types;
mod validation;

pub use ast::{Expr, Statement, StatementKind};
pub use evaluator::{evaluate_root, EvalContext, RuntimeFunction};
pub use library::{Library, LibraryError};
pub use parser::{parse, StreamParser};
pub use prompt::{system_prompt, PromptOptions};
pub use scanner::{scan_blocks, GenUiBlock};
pub use types::{Action, Element, ErrorCode, EvalError, EvalResult, ParseError, ParseResult};

pub type Result<T> = std::result::Result<T, EvalError>;

// 限制输入递归及展开预算，避免模型输出把进程栈或内存耗尽。
pub(crate) const MAX_DEPTH: usize = 64;
pub(crate) const MAX_NODES: usize = 100_000;
// action schema 是配置输入；共享预算防止双引用链在编译期指数展开。
pub(crate) const MAX_SCHEMA_EXPANSIONS: usize = 4_096;
// 生成值上限按 100k AST 节点的结构开销估算；state 总量允许四个满额值。
pub(crate) const MAX_VALUE_BYTES: usize = 4 << 20;
pub(crate) const MAX_STATE_BYTES: usize = 16 << 20;
