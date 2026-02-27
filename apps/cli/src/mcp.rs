use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;
use tower_mcp::error::ResultExt;
use tower_mcp::transport::stdio::StdioTransport;
use tower_mcp::{CallToolResult, McpRouter, ToolBuilder};
use trivia_core::{Embedder, Memory, MemoryStore, TriviaConfig};

struct AppState {
    store: Mutex<MemoryStore>,
    embedder: Mutex<Embedder>,
    config: TriviaConfig,
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
    /// Maximum number of results (default: 5, max: 10)
    limit: Option<usize>,
    /// Optional tag filter — only return memories with at least one matching tag
    tags: Option<Vec<String>>,
    /// Minimum composite score threshold — filter out low-relevance results
    min_score: Option<f64>,
    /// Boost results containing this text in mnemonic or body. Prefer short, specific strings.
    full_text_search: Option<String>,
    /// Exclude memories with any of these tags
    exclude_tags: Option<Vec<String>>,
    /// Maximum body characters to return per memory (truncates with "... (N more chars)")
    truncate: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MergeInput {
    /// Mnemonic of the memory to keep
    keep: String,
    /// Mnemonic of the memory to absorb and delete
    discard: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExportInput {
    /// Directory to export memories to
    directory: String,
    /// Optional tag filter — only export memories with at least one matching tag
    #[serde(default)]
    tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ImportInput {
    /// Directory to import memories from
    directory: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RateInput {
    /// Mnemonic of a single memory to rate (use this or mnemonics, not both)
    mnemonic: Option<String>,
    /// Mnemonics of multiple memories to rate at once
    mnemonics: Option<Vec<String>>,
    /// Whether the memory was useful (true) or not useful (false)
    useful: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LinkInput {
    /// Mnemonic of the source memory
    source: String,
    /// Mnemonic of the target memory
    target: String,
    /// Type of link: "related", "supersedes", or "derived_from"
    link_type: String,
}

fn format_memories(memories: &[Memory], truncate: Option<usize>) -> String {
    let mut output = String::new();
    for (i, mem) in memories.iter().enumerate() {
        output.push_str(&format!(
            "{}. [{}] (score: {:.4})\n",
            i + 1,
            mem.mnemonic,
            mem.score,
        ));
        output.push_str(&format!(
            "   created: {} | updated: {} | recalled: {} times\n",
            mem.created_at.format("%Y-%m-%dT%H:%M:%SZ"),
            mem.updated_at.format("%Y-%m-%dT%H:%M:%SZ"),
            mem.recall_count,
        ));
        if !mem.tags.is_empty() {
            output.push_str(&format!("   tags: {}\n", mem.tags.join(", ")));
        }
        if !mem.links.is_empty() {
            let link_strs: Vec<String> = mem
                .links
                .iter()
                .map(|l| {
                    let other = if l.source_mnemonic == mem.mnemonic {
                        &l.target_mnemonic
                    } else {
                        &l.source_mnemonic
                    };
                    format!("{} ({})", other, l.link_type)
                })
                .collect();
            output.push_str(&format!("   links: {}\n", link_strs.join(", ")));
        }
        output.push('\n');

        // Body with optional truncation
        let body = &mem.content;
        match truncate {
            Some(max) if body.chars().count() > max => {
                let truncated: String = body.chars().take(max).collect();
                let remaining = body.chars().count() - max;
                output.push_str(&format!("{}... ({} more chars)\n", truncated, remaining));
            }
            _ => {
                output.push_str(body);
                output.push('\n');
            }
        }
        output.push('\n');
    }
    output
}

pub async fn serve(store: MemoryStore, embedder: Embedder, config: TriviaConfig) -> Result<()> {
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        embedder: Mutex::new(embedder),
        config,
    });

    let app = state.clone();
    let memorize = ToolBuilder::new("memorize")
        .description("Store a fact or context for later recall. Use a mnemonic (file path, concept, phrase) plus the content to remember. Good examples include \"project design\"; \"feedback on src/files/foo.rs\"; \"implementation of the Frobnicator Component\". You're looking to capture what the memory was about with the mnemonic rather than the content of the memory.")
        .handler(move |input: MemorizeInput| {
            let app = app.clone();
            async move {
                let tags = TriviaConfig::merge_tags(&app.config.memorize.tags, &input.tags);
                let embedding = app.embedder.lock().await.embed(&input.mnemonic)
                    .tool_context("embedding failed")?;
                app.store.lock().await
                    .memorize(&input.mnemonic, &input.content, &tags, &embedding)
                    .tool_context("memorize failed")?;
                Ok(CallToolResult::text(format!("Memorized: {}", input.mnemonic)))
            }
        })
        .build();

    let app = state.clone();
    let recall = ToolBuilder::new("recall")
        .description("Retrieve previously memorized facts by semantic similarity. Provide a natural language query describing what you're looking for. Use full_text_search to boost results containing specific keywords. Use min_score to filter low-relevance results. Use exclude_tags to hide irrelevant categories.")
        .handler(move |input: RecallInput| {
            let app = app.clone();
            async move {
                let embedding = app.embedder.lock().await.embed(&input.query)
                    .tool_context("embedding failed")?;
                let limit = input.limit.unwrap_or(5).clamp(1, 10);
                let tags = input.tags.as_deref();
                let fts = input.full_text_search.as_deref();
                let exclude = input.exclude_tags.as_deref();
                let mut memories = app.store.lock().await
                    .recall(&embedding, limit, tags, fts, exclude)
                    .tool_context("recall failed")?;

                // Apply min_score: param > config > 0.0
                let min_score = input.min_score
                    .or(app.config.recall.min_score)
                    .unwrap_or(0.0);
                memories.retain(|m| m.score >= min_score);

                if memories.is_empty() {
                    return Ok(CallToolResult::text("No memories found."));
                }

                let truncate = input.truncate.or(app.config.recall.body_max_chars);
                Ok(CallToolResult::text(format_memories(&memories, truncate)))
            }
       })
        .build();

    let app = state.clone();
    let rate = ToolBuilder::new("rate")
        .description("Rate previously recalled memories as useful or not useful. Accepts a single mnemonic or a batch of mnemonics. Silent on complete success; reports only not-found mnemonics.")
        .handler(move |input: RateInput| {
            let app = app.clone();
            async move {
                // Merge single + batch mnemonics
                let mut all = input.mnemonics.unwrap_or_default();
                if let Some(single) = input.mnemonic {
                    if !all.contains(&single) {
                        all.insert(0, single);
                    }
                }
                if all.is_empty() {
                    return Err(anyhow::anyhow!("provide mnemonic or mnemonics"))
                        .tool_context("rate failed");
                }

                let not_found = app.store
                    .lock()
                    .await
                    .rate_batch(&all, input.useful)
                    .tool_context("rate failed")?;

                if not_found.is_empty() {
                    Ok(CallToolResult::text(String::new()))
                } else {
                    let msg = not_found.iter()
                        .map(|m| format!("Not found: {}", m))
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(CallToolResult::text(msg))
                }
            }
        })
        .build();

    let app = state.clone();
    let link = ToolBuilder::new("link")
        .description("Create a link between two memories. Link types: \"related\", \"supersedes\", \"derived_from\".")
        .handler(move |input: LinkInput| {
            let app = app.clone();
            async move {
                app.store
                    .lock()
                    .await
                    .link(&input.source, &input.target, &input.link_type)
                    .tool_context("link failed")?;
                Ok(CallToolResult::text(format!(
                    "Linked: {} --[{}]--> {}",
                    input.source, input.link_type, input.target
                )))
            }
        })
        .build();

    let app = state.clone();
    let merge = ToolBuilder::new("merge")
        .description("Merge two memories: keep absorbs discard's content, tags, and links. The discard memory is deleted.")
        .handler(move |input: MergeInput| {
            let app = app.clone();
            async move {
                let embedding = app
                    .embedder
                    .lock()
                    .await
                    .embed(&input.keep)
                    .tool_context("embedding failed")?;
                app.store
                    .lock()
                    .await
                    .merge(&input.keep, &input.discard, &embedding)
                    .tool_context("merge failed")?;
                Ok(CallToolResult::text(format!(
                    "Merged: {} absorbed {}",
                    input.keep, input.discard
                )))
            }
        })
        .build();

    let app = state.clone();
    let export = ToolBuilder::new("export")
        .description("Export memories to a directory as markdown files with YAML frontmatter. Optionally filter by tags.")
        .handler(move |input: ExportInput| {
            let app = app.clone();
            async move {
                let dir = std::path::Path::new(&input.directory);
                let tags = input.tags.as_deref();
                app.store
                    .lock()
                    .await
                    .export(dir, tags)
                    .tool_context("export failed")?;
                Ok(CallToolResult::text(format!(
                    "Exported to: {}",
                    input.directory
                )))
            }
        })
        .build();

    let app = state.clone();
    let import = ToolBuilder::new("import")
        .description("Import memories from a directory of markdown files with YAML frontmatter.")
        .handler(move |input: ImportInput| {
            let app = app.clone();
            async move {
                let dir = std::path::Path::new(&input.directory);
                let embedder = app.embedder.lock().await;
                let result = app
                    .store
                    .lock()
                    .await
                    .import(dir, &embedder)
                    .tool_context("import failed")?;
                Ok(CallToolResult::text(format!(
                    "Imported: {} created, {} updated, {} unchanged",
                    result.created, result.updated, result.unchanged
                )))
            }
        })
        .build();

    let app = state.clone();
    let list_tags = ToolBuilder::new("list-tags")
        .description("List all unique tags with the number of memories using each tag.")
        .handler(move |_input: Option<()>| {
            let app = app.clone();
            async move {
                let tags = app
                    .store
                    .lock()
                    .await
                    .list_tags()
                    .tool_context("list-tags failed")?;

                if tags.is_empty() {
                    return Ok(CallToolResult::text("No tags found."));
                }

                let mut output = String::new();
                for t in &tags {
                    output.push_str(&format!("{} ({} memories)\n", t.tag, t.count));
                }
                Ok(CallToolResult::text(output))
            }
        })
        .build();

    let router = McpRouter::new()
        .server_info("trivia", "0.1.0")
        .instructions("Semantic memory store. Use `memorize` to save facts with a mnemonic identifier, `recall` to retrieve them by semantic similarity (supports `full_text_search` for keyword boosting, `min_score` for relevance filtering, `exclude_tags` to hide categories, and `truncate` for body length), `rate` to mark recalled memories as useful or not (accepts batch via `mnemonics` array), `link` to create explicit links between memories, `merge` to consolidate near-duplicate memories, `export` to save to files, `import` to load from files, and `list-tags` to see all tags.")
        .tool(memorize)
        .tool(recall)
        .tool(rate)
        .tool(link)
        .tool(merge)
        .tool(export)
        .tool(import)
        .tool(list_tags);

    StdioTransport::new(router).run().await?;
    Ok(())
}
