//! MCP server exposing oxcode's read-only code-intelligence queries over stdio.
//!
//! The primary tool is `oxcode_explore`: it returns the bounded, PageRank-curated
//! context for a question in one call, so an agent can answer without composing a
//! grep/read loop. Lower-level tools (`oxcode_search`, `oxcode_callers`, …) cover
//! targeted follow-ups. stdout carries the JSON-RPC stream; all logging is stderr.

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};

mod server;

#[tokio::main]
async fn main() -> Result<()> {
    let service = server::OxcodeServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
