use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;
use tower_mcp::error::ResultExt;
use tower_mcp::extract::{Extension, Json, State};
use tower_mcp::oauth::token::TokenClaims;
use tower_mcp::transport::stdio::StdioTransport;
use tower_mcp::{CallToolResult, McpRouter, ToolBuilder};
use trivia_core::{Embedder, Memory, MemoryStore, MemorizeResult, TriviaConfig};

use crate::acl::Acl;
use crate::auth_middleware::acl_from_claims;

struct AppState {
    store: Arc<Mutex<MemoryStore>>,
    embedder: Arc<Mutex<Embedder>>,
    config: TriviaConfig,
    acl: Arc<Acl>,
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
    /// Natural language query — matched by meaning, not keywords. Describe what you're looking for.
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
struct EditInput {
    /// Current mnemonic of the memory to edit
    mnemonic: String,
    /// New mnemonic (will re-embed the memory)
    new_mnemonic: Option<String>,
    /// Tags to add
    #[serde(default)]
    add_tags: Vec<String>,
    /// Tags to remove
    #[serde(default)]
    remove_tags: Vec<String>,
    /// Additional mnemonic aliases to add (each gets its own embedding for recall)
    #[serde(default)]
    add_mnemonics: Vec<String>,
    /// Mnemonic aliases to remove
    #[serde(default)]
    remove_mnemonics: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RenameTagInput {
    /// Current tag name
    old_tag: String,
    /// New tag name
    new_tag: String,
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

fn format_memorize_response(mnemonic: &str, result: &MemorizeResult) -> String {
    let mut output = if let Some(ref merged) = result.merged_with {
        format!("Memorized and merged with: {merged}")
    } else {
        format!("Memorized: {mnemonic}")
    };

    if !result.neighbors.is_empty() {
        output.push_str("\n\nNearby memories:");
        for n in &result.neighbors {
            let tags_str = if n.tags.is_empty() {
                String::new()
            } else {
                format!(", tags: [{}]", n.tags.join(", "))
            };
            output.push_str(&format!(
                "\n  - \"{}\" (distance: {:.2}{tags_str})",
                n.mnemonic, n.distance
            ));
        }

        // Warn about very close neighbors
        let close: Vec<&trivia_core::MemorizeNeighbor> = result
            .neighbors
            .iter()
            .filter(|n| n.distance < 0.22)
            .collect();
        if !close.is_empty() {
            output.push_str("\n\nNote: ");
            for (i, n) in close.iter().enumerate() {
                if i > 0 {
                    output.push_str(", ");
                }
                output.push_str(&format!("\"{}\" (distance {:.2})", n.mnemonic, n.distance));
            }
            let verb = if close.len() == 1 { "is" } else { "are" };
            output.push_str(&format!(
                " {verb} fairly close. If these memories compete in future recalls, \
                 consider using `edit` to disambiguate their mnemonics.",
            ));
        }
    }

    output
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
        if mem.mnemonics.len() > 1 {
            let aliases: Vec<&str> = mem.mnemonics.iter()
                .filter(|m| m.as_str() != mem.mnemonic)
                .map(|m| m.as_str())
                .collect();
            if !aliases.is_empty() {
                output.push_str(&format!("   aliases: {}\n", aliases.join(", ")));
            }
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

/// Helper: look up a memory's tags by mnemonic. Returns None if not found.
async fn memory_tags(store: &Arc<Mutex<MemoryStore>>, mnemonic: &str) -> Result<Option<Vec<String>>> {
    let s = store.lock().await;
    match s.get_memory_by_mnemonic(mnemonic)? {
        Some(mem) => Ok(Some(mem.tags)),
        None => Ok(None),
    }
}

/// Build the MCP router with ACL enforcement.
pub fn build_mcp_router(
    store: Arc<Mutex<MemoryStore>>,
    embedder: Arc<Mutex<Embedder>>,
    config: TriviaConfig,
    acl: Arc<Acl>,
) -> McpRouter {
    let state = Arc::new(AppState { store, embedder, config, acl });
    build_router(state)
}

/// Serve MCP over stdio (no ACL restrictions).
pub async fn serve(store: MemoryStore, embedder: Embedder, config: TriviaConfig) -> Result<()> {
    let state = Arc::new(AppState {
        store: Arc::new(Mutex::new(store)),
        embedder: Arc::new(Mutex::new(embedder)),
        config,
        acl: Arc::new(Acl::open()),
    });

    let router = build_router(state);
    StdioTransport::new(router).run().await?;
    Ok(())
}

fn build_router(state: Arc<AppState>) -> McpRouter {
    let s = state.clone();
    let memorize = ToolBuilder::new("memorize")
        .description("Store a fact or context for later recall. Use a mnemonic (file path, concept, phrase) plus the content to remember. Good examples include \"project design\"; \"feedback on src/files/foo.rs\"; \"implementation of the Frobnicator Component\". You're looking to capture what the memory was about with the mnemonic rather than the content of the memory. Mnemonics that are very similar to existing ones may be auto-merged. The response will note nearby memories and warn about close collisions.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<MemorizeInput>| async move {
                let (acl, username) = acl_from_claims(&claims, &app.acl);
                let mut tags = TriviaConfig::merge_tags(&app.config.memorize.tags, &input.tags);

                // Auto-tag with @username when authenticated
                if let Some(ref name) = username {
                    let user_tag = format!("@{name}");
                    if !tags.contains(&user_tag) {
                        tags.push(user_tag);
                    }
                }

                // ACL: at least one tag must grant update
                if !acl.is_open() && !acl.check_update(&tags) {
                    return Err(anyhow::anyhow!(
                        "access denied: your permissions are [{}] which do not grant update access",
                        acl
                    ))
                    .tool_context("memorize denied");
                }

                let skip_merge = !acl.is_open();
                let embedding = app.embedder.lock().await.embed(&input.mnemonic)
                    .tool_context("embedding failed")?;
                let result = app.store.lock().await
                    .memorize_with_options(&input.mnemonic, &input.content, &tags, &embedding, skip_merge)
                    .tool_context("memorize failed")?;
                Ok(CallToolResult::text(format_memorize_response(&input.mnemonic, &result)))
            },
        )
        .build();

    let s = state.clone();
    let recall = ToolBuilder::new("recall")
        .description("Retrieve previously memorized facts by semantic similarity. Provide a natural language query describing what you're looking for. Use `full_text_search` alongside it to boost results that contain a specific keyword or phrase — this is useful when you know the exact term but want semantic ranking too. Use min_score to filter low-relevance results. Use exclude_tags to hide irrelevant categories.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<RecallInput>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
                let embedding = app.embedder.lock().await.embed(&input.query)
                    .tool_context("embedding failed")?;
                let limit = input.limit.unwrap_or(5).clamp(1, 10);
                let tags = input.tags.as_deref();
                let fts = input.full_text_search.as_deref();
                let exclude = input.exclude_tags.as_deref();
                let mut memories = app.store.lock().await
                    .recall(&embedding, limit, tags, fts, exclude)
                    .tool_context("recall failed")?;

                // ACL: post-filter by read access
                if !acl.is_open() {
                    memories.retain(|m| acl.check_read(&m.tags));
                }

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
            },
        )
        .build();

    let s = state.clone();
    let rate = ToolBuilder::new("rate")
        .description("Rate previously recalled memories as useful or not useful. Call this after using recalled memories to improve future ranking. Accepts a single mnemonic or a batch of mnemonics. Silent on complete success; reports only not-found mnemonics.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<RateInput>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
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

                // ACL: each memory must grant update
                if !acl.is_open() {
                    for mn in &all {
                        if let Some(tags) = memory_tags(&app.store, mn).await
                            .tool_context("rate failed")? {
                            if !acl.check_update(&tags) {
                                return Err(anyhow::anyhow!(
                                    "access denied: your permissions are [{}] which do not grant update access", acl
                                )).tool_context("rate denied");
                            }
                        }
                    }
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
            },
        )
        .build();

    let s = state.clone();
    let link = ToolBuilder::new("link")
        .description("Create a link between two memories. Link types: \"related\" (topically connected), \"supersedes\" (source replaces target — used after corrections or updates), \"derived_from\" (source was created based on target).")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<LinkInput>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
                // ACL: both memories must grant update
                if !acl.is_open() {
                    for mn in [&input.source, &input.target] {
                        if let Some(tags) = memory_tags(&app.store, mn).await
                            .tool_context("link failed")? {
                            if !acl.check_update(&tags) {
                                return Err(anyhow::anyhow!(
                                    "access denied: your permissions are [{}] which do not grant update access", acl
                                )).tool_context("link denied");
                            }
                        }
                    }
                }

                app.store
                    .lock()
                    .await
                    .link(&input.source, &input.target, &input.link_type)
                    .tool_context("link failed")?;
                Ok(CallToolResult::text(format!(
                    "Linked: {} --[{}]--> {}",
                    input.source, input.link_type, input.target
                )))
            },
        )
        .build();

    let s = state.clone();
    let merge = ToolBuilder::new("merge")
        .description("Merge two memories: keep absorbs discard's content, tags, and links. The discard memory is deleted. Use when two memories cover the same topic and should be one entry. Prefer `link` over merge when memories are related but distinct.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<MergeInput>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
                // ACL: both memories must grant update
                if !acl.is_open() {
                    for mn in [&input.keep, &input.discard] {
                        if let Some(tags) = memory_tags(&app.store, mn).await
                            .tool_context("merge failed")? {
                            if !acl.check_update(&tags) {
                                return Err(anyhow::anyhow!(
                                    "access denied: your permissions are [{}] which do not grant update access", acl
                                )).tool_context("merge denied");
                            }
                        }
                    }
                }

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
            },
        )
        .build();

    let s = state.clone();
    let export = ToolBuilder::new("export")
        .description("Export memories to a directory as markdown files with YAML frontmatter. Optionally filter by tags.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<ExportInput>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
                let dir = std::path::Path::new(&input.directory);
                let tags = input.tags.as_deref();

                if acl.is_open() {
                    app.store.lock().await
                        .export(dir, tags)
                        .tool_context("export failed")?;
                } else {
                    // ACL: only export readable memories
                    let acl = acl.clone();
                    app.store.lock().await
                        .export_filtered(dir, tags, move |mem_tags| acl.check_read(mem_tags))
                        .tool_context("export failed")?;
                }

                Ok(CallToolResult::text(format!("Exported to: {}", input.directory)))
            },
        )
        .build();

    let s = state.clone();
    let import = ToolBuilder::new("import")
        .description("Import memories from a directory of markdown files with YAML frontmatter.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<ImportInput>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
                // ACL: import is blocked in shared mode
                if !acl.is_open() {
                    return Err(anyhow::anyhow!("import is disabled in shared mode"))
                        .tool_context("import denied");
                }

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
            },
        )
        .build();

    let s = state.clone();
    let list_tags = ToolBuilder::new("list-tags")
        .description("List all unique tags with the number of memories using each tag.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
                let tags = app
                    .store
                    .lock()
                    .await
                    .list_tags()
                    .tool_context("list-tags failed")?;

                // ACL: post-filter by read access
                let tags: Vec<_> = if acl.is_open() {
                    tags
                } else {
                    tags.into_iter()
                        .filter(|t| acl.tag_level(&t.tag) >= crate::acl::AccessLevel::Read)
                        .collect()
                };

                if tags.is_empty() {
                    return Ok(CallToolResult::text("No tags found."));
                }

                let mut output = String::new();
                for t in &tags {
                    output.push_str(&format!("{} ({} memories)\n", t.tag, t.count));
                }
                Ok(CallToolResult::text(output))
            },
        )
        .build();

    let s = state.clone();
    let edit = ToolBuilder::new("edit")
        .description("Edit an existing memory's mnemonic or tags. Use to disambiguate colliding mnemonics or fix tag assignments. If the mnemonic is changed, the memory will be re-embedded. Use add_mnemonics/remove_mnemonics to manage additional aliases — each alias gets its own embedding vector for improved recall coverage.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<EditInput>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
                if input.new_mnemonic.is_none() && input.add_tags.is_empty() && input.remove_tags.is_empty()
                    && input.add_mnemonics.is_empty() && input.remove_mnemonics.is_empty() {
                    return Err(anyhow::anyhow!("provide at least one of: new_mnemonic, add_tags, remove_tags, add_mnemonics, remove_mnemonics"))
                        .tool_context("edit failed");
                }

                // ACL: memory's current tags must grant update
                if !acl.is_open() {
                    if let Some(tags) = memory_tags(&app.store, &input.mnemonic).await
                        .tool_context("edit failed")? {
                        if !acl.check_update(&tags) {
                            return Err(anyhow::anyhow!(
                                "access denied: your permissions are [{}] which do not grant update access",
                                acl
                            )).tool_context("edit denied");
                        }
                    }
                }

                let embedder = app.embedder.lock().await;
                let new_embedding = match &input.new_mnemonic {
                    Some(mn) => Some(
                        embedder.embed(mn).tool_context("embedding failed")?
                    ),
                    None => None,
                };
                // Embed each new mnemonic alias
                let mut mnemonic_embeddings: Vec<Vec<f32>> = Vec::new();
                for mn in &input.add_mnemonics {
                    mnemonic_embeddings.push(
                        embedder.embed(mn).tool_context("embedding failed")?
                    );
                }
                drop(embedder);

                let result = app.store.lock().await
                    .edit_memory(
                        &input.mnemonic,
                        input.new_mnemonic.as_deref(),
                        &input.add_tags,
                        &input.remove_tags,
                        new_embedding.as_deref(),
                        &input.add_mnemonics,
                        &input.remove_mnemonics,
                        &mnemonic_embeddings,
                    )
                    .tool_context("edit failed")?;

                let mut output = if result.re_embedded {
                    format!("Renamed: \"{}\" -> \"{}\"", result.old_mnemonic, result.new_mnemonic)
                } else {
                    format!("Updated: \"{}\"", result.old_mnemonic)
                };
                if !result.tags.is_empty() {
                    output.push_str(&format!("\nTags: [{}]", result.tags.join(", ")));
                }
                if result.mnemonics.len() > 1 {
                    output.push_str(&format!("\nMnemonics: [{}]", result.mnemonics.join(", ")));
                }
                Ok(CallToolResult::text(output))
            },
        )
        .build();

    let s = state.clone();
    let rename_tag = ToolBuilder::new("rename-tag")
        .description("Rename a tag across all memories. No re-embedding needed.")
        .extractor_handler(
            s,
            |State(app): State<Arc<AppState>>,
             Extension(claims): Extension<TokenClaims>,
             Json(input): Json<RenameTagInput>| async move {
                let (acl, _username) = acl_from_claims(&claims, &app.acl);
                // ACL: both old and new tag must grant update
                if !acl.is_open() {
                    if acl.tag_level(&input.old_tag) < crate::acl::AccessLevel::Update {
                        return Err(anyhow::anyhow!(
                            "access denied: your permissions are [{}] which do not grant update access",
                            acl
                        )).tool_context("rename-tag denied");
                    }
                    if acl.tag_level(&input.new_tag) < crate::acl::AccessLevel::Update {
                        return Err(anyhow::anyhow!(
                            "access denied: your permissions are [{}] which do not grant update access",
                            acl
                        )).tool_context("rename-tag denied");
                    }
                }

                let count = app.store.lock().await
                    .rename_tag(&input.old_tag, &input.new_tag)
                    .tool_context("rename-tag failed")?;
                Ok(CallToolResult::text(format!(
                    "Renamed tag \"{}\" -> \"{}\" across {count} memories",
                    input.old_tag, input.new_tag
                )))
            },
        )
        .build();

    // Default TokenClaims for stdio/test (no HTTP middleware).
    // For HTTP, the auth middleware inserts per-request claims which override this.
    let default_claims = crate::auth_middleware::default_claims(&state.acl);

    McpRouter::new()
        .server_info("trivia", "0.1.0")
        .instructions("Semantic memory store. Memories are keyed by a short mnemonic (a concept name, file path, or phrase) and hold longer-form content. The mnemonic is what gets embedded for vector search; content is searched via full-text keyword match.\n\nTypical workflow: `recall` before starting work to load relevant context, `memorize` to save new facts, `rate` results after using them so ranking improves over time. Very similar mnemonics are auto-merged on memorize (distance < 0.15) and auto-linked (distance < 0.30).\n\nUse `edit` to rename mnemonics that collide in recall. Use `merge` when two memories cover the same topic. Use `link` to create explicit relationships between distinct memories.\n\nMemories can have multiple mnemonic aliases via `edit`'s `add_mnemonics`/`remove_mnemonics` parameters. Each alias gets its own embedding vector, increasing recall surface area when different phrasings are used.")
        .with_state(default_claims)
        .tool(memorize)
        .tool(recall)
        .tool(rate)
        .tool(link)
        .tool(merge)
        .tool(edit)
        .tool(rename_tag)
        .tool(export)
        .tool(import)
        .tool(list_tags)
}

#[cfg(test)]
mod tests {
    use schemars::schema_for;
    use tower_mcp::NoParams;

    #[test]
    fn no_params_schema_is_valid_mcp_object() {
        let schema = schema_for!(NoParams);
        let value = serde_json::to_value(&schema).unwrap();
        assert_eq!(value["type"], "object");
        // MCP requires "type": "object", not "type": "null"
        assert_ne!(value["type"], "null");
    }
}
