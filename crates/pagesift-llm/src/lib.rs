//! OpenAI 兼容的 Chat Completions 客户端，以及基于它的结构化提取和摘要。
//!
//! 不内置任何 API key 或默认模型名，全部由调用方配置。

#![forbid(unsafe_code)]

mod client;
mod error;
mod extract;
mod schema;
mod summary;

pub use client::{
    ChatClient, ChatMessage, ChatRequest, ChatResponse, LlmConfig, ResponseFormat, Role, SchemaMode,
};
pub use error::{LlmError, Result};
pub use extract::{ExtractOptions, extract_json};
pub use schema::validate as validate_schema;
pub use summary::{SummaryOptions, estimate_tokens, summarize};
