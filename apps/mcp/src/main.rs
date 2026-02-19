use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;
use tower_mcp::error::ResultExt;
use tower_mcp::transport::stdio::StdioTransport;
use tower_mcp::{CallToolResult, McpRouter, ToolBuilder};
use trivia_core::{Embedder, MemoryStore};

struct AppState {
    store: Mutex<MemoryStore>,
    embedder: Mutex<Embedder>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MemorizeInput {
    /// Short identifier: a file path, concept name, or phrase
    mnemonic: String,
    /// The fact or context to remember
    content: String,
    /// Optional categorization tags
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RecallInput {
    /// Natural language search query
    query: String,
    /// Maximum number of results (default: 5)
    limit: Option<usize>,
    /// Optional tag filter
    tags: Option<Vec<String>>,
}

fn db_path() -> PathBuf {
    if let Ok(path) = std::env::var("TRIVIA_DB") {
        PathBuf::from(path)
    } else {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".claude")
            .join("trivia.db")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let store = MemoryStore::new(&db_path())?;
    let embedder = Embedder::new()?;
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        embedder: Mutex::new(embedder),
    });

    let app = state.clone();
    let memorize = ToolBuilder::new("memorize")
        .description("Store a fact or context for later recall. Use a mnemonic (file path, concept, phrase) plus the content to remember. Good examples include \"project design\"; \"feedback on src/files/foo.rs\"; \"implementation of the Frobnicator Component\". You're looking to capture what the memory was about with the mnemonic rather than the content of the memory.")
        .handler(move |input: MemorizeInput| {
            let app = app.clone();
            async move {
                let embedding = app.embedder.lock().await.embed(&input.mnemonic)
                    .tool_context("embedding failed")?;
                app.store.lock().await
                    .memorize(&input.mnemonic, &input.content, &input.tags, &embedding)
                    .tool_context("memorize failed")?;
                Ok(CallToolResult::text(format!("Memorized: {}", input.mnemonic)))
            }
        })
        .build();
    let app = state.clone();
    let recall = ToolBuilder::new("recall")
        .description("Retrieve previously memorized facts by semantic similarity. Provide a natural language query describing what you're looking for.")
        .handler(move |input: RecallInput| {
            let app = app.clone();
            async move {
                let embedding = app.embedder.lock().await.embed(&input.query)
                    .tool_context("embedding failed")?;
                let limit = input.limit.unwrap_or(5);
                let tags = input.tags.as_deref();
                let memories = app.store.lock().await.recall(&embedding, limit, tags)
                    .tool_context("recall failed")?;

                if memories.is_empty() {
                    return Ok(CallToolResult::text("No memories found."));
                }

                let mut output = String::new();
                for (i, mem) in memories.iter().enumerate() {
                    output.push_str(&format!(
                        "{}. [{}] (distance: {:.4}, recalled: {} times)\n{}\n",
                        i + 1,
                        mem.mnemonic,
                        mem.distance,
                        mem.recall_count,
                        mem.content,
                    ));
                    if !mem.tags.is_empty() {
                        output.push_str(&format!("   tags: {}\n", mem.tags.join(", ")));
                    }
                    output.push('\n');
                }
                Ok(CallToolResult::text(output))
            }
       })
        .build();

    let router = McpRouter::new()
        .server_info("trivia", "0.1.0")
        .instructions("Semantic memory store. Use `memorize` to save facts with a mnemonic identifier, and `recall` to retrieve them by semantic similarity.")
        .tool(memorize)
        .tool(recall);

    StdioTransport::new(router).run().await?;
    Ok(())
}
