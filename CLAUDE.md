# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Trivia?

A semantic memory store for Claude Code. Memorize facts with a mnemonic key, recall them by meaning using vector embeddings. Runs as a CLI, an MCP server (stdin/stdout JSON-RPC), and a web UI.

## Commands

```bash
just build          # cargo build --release
just test           # cargo test --workspace
just www            # serve web UI (Axum, default port 3000)
just vite           # React dev server (Vite, proxies to Axum API)
just dev            # www + vite in parallel
```

Build the web UI before `cargo build` if `apps/cli/www/dist/` is missing:
```bash
cd apps/cli/www && npm ci && npm run build
```

Run a single test:
```bash
cargo test -p trivia-core -- test_name
```

## Architecture

**Workspace:** two members — `crates/core` (library) and `apps/cli` (binary, named `trivia`).

### `crates/core` (trivia-core)

- **`store.rs`** — `MemoryStore` wrapping a single `rusqlite::Connection`. SQLite schema: `memories` table, `memory_vectors` (sqlite-vec, float[384] KNN via L2), `memory_links` (typed edges), `memory_fts` (FTS5 with porter stemmer, synced via triggers). Additive migration — no version table, `ADD COLUMN IF NOT EXISTS` pattern.
- **`embedder.rs`** — `Embedder` wrapping `fastembed::TextEmbedding` (AllMiniLM-L6-V2, 384 dims). Embeds the **mnemonic**, not the content.
- **`config.rs`** — `TriviaConfig` from `trivia.toml`, discovered by walking up from `CLAUDE_PLUGIN_ROOT` or CWD.
- **`export.rs`** — Markdown + YAML frontmatter export/import. Two-pass import (memories first, then links by UUID).

### `apps/cli` (trivia-cli → binary `trivia`)

- **`main.rs`** — Clap CLI. Auto-detects MCP mode when stdin is not a TTY and no args given. DB path: `TRIVIA_DB` env > config > `~/.claude/trivia.db`.
- **`mcp.rs`** — MCP server via `tower-mcp` + `StdioTransport`. Tools: memorize, recall, rate, link, merge, edit, rename-tag, export, import, list-tags. Uses `Arc<Mutex<MemoryStore>>` / `Arc<Mutex<Embedder>>` for async.
- **`www.rs`** — Axum REST API. Static files embedded at compile time via `include_dir!("www/dist")` with SPA fallback.

### `apps/cli/www` (React web UI)

React 19 + TypeScript + Vite + Tailwind 4 + React Query + React Router 7 + d3 (force graph).

## Key Design Details

- **Composite recall scoring:** similarity (1.0), FTS (0.5), tag boost (0.2), rating (0.15), recency (0.1, 7-day half-life), link (0.1), frequency (0.05).
- **Auto-merge threshold:** 0.15 distance on memorize. **Auto-link threshold:** 0.30.
- **Tags** stored as JSON arrays in SQLite; queried with `json_each()`.
- **UUIDs** preserved across upserts for export/import roundtrip fidelity.
- Core is synchronous `rusqlite`; async servers wrap in `tokio::sync::Mutex`.
