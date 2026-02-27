# trivia

Semantic memory for Claude Code. Memorize facts, recall them by meaning, and let connections form automatically.

## Quick Start

```bash
# Install (from source for now)
cargo install --path apps/cli

# As a Claude Code plugin
claude plugin add chrisdickinson/trivia
claude plugin install trivia
```

### Basic Usage

```bash
# Store a fact
trivia memorize "project architecture" "Three-layer: API, service, storage. Each layer in its own crate."

# Recall by meaning
trivia recall "how is the code organized"

# Tag for organization
trivia memorize "auth flow" "OAuth2 PKCE with JWT refresh tokens" --tag backend --tag auth
```

### As an MCP Tool

When installed as a Claude Code plugin, trivia is available as an MCP server. Claude can `memorize` and `recall` facts during conversations.

## Features

- **Semantic search** via embeddings (AllMiniLM-L6-V2, 384-dim)
- **Auto-linking** — similar memories are linked automatically
- **Auto-merging** — very similar memories merge on creation
- **Manual links** — `related`, `supersedes`, `derived_from`
- **Composite scoring** — similarity + recency + frequency + link boost + ratings
- **Tagging** — categorize and filter memories
- **Rating feedback** — mark memories as useful/not to improve ranking
- **Export/Import** — markdown files with YAML frontmatter
- **Web UI** — browse, edit, search, graph visualization
- **MCP server** — Claude Code integration via stdin/stdout JSON-RPC

## CLI Reference

```
trivia memorize <mnemonic> <content> [--tag <tag>...]
trivia recall <query> [--limit N] [--tag <tag>...] [--json]
trivia link <source> <target> [--link-type related|supersedes|derived_from]
trivia links <mnemonic>
trivia merge <keep> <discard>
trivia rate <mnemonic> --useful|--not-useful
trivia export <directory> [--tag <tag>...]
trivia import <directory>
trivia list-tags [--json]
trivia automerge [--threshold 0.25] [--dry-run]
trivia www [--port 3000]
trivia mcp
```

## MCP Tools

| Tool | Description |
|------|-------------|
| `memorize` | Store a fact with mnemonic, content, and optional tags |
| `recall` | Search by semantic similarity |
| `rate` | Provide useful/not-useful feedback |
| `link` | Create typed connections between memories |
| `merge` | Consolidate duplicate memories |
| `export` | Save memories to markdown files (optional tag filter) |
| `import` | Load memories from markdown files |
| `list-tags` | List all tags with counts |

## Configuration

Create a `trivia.toml` in your project root:

```toml
# Auto-add these tags to every memorize call
[memorize]
tags = ["my-project", "backend"]

# Boost these tags in recall scoring (not a filter — all memories still searchable)
[recall]
tags = ["my-project"]

# Default tag filter for export
[export]
tags = ["my-project"]

# Optional: override database path (default: ~/.claude/trivia.db)
# database = "/path/to/trivia.db"
```

Config discovery walks up from CWD (or `CLAUDE_PLUGIN_ROOT`) to find the nearest `trivia.toml`. CLI flags are additive with config tags.

## Web UI

Start with `trivia www` and open `http://localhost:3000`. Features:

- Memory list with search and tag filtering
- Memory detail view with inline editing (including mnemonic rename)
- Link management
- Interactive merge
- Force-directed graph visualization

## Architecture

```
crates/core/     — MemoryStore, Embedder, config, export/import
apps/cli/        — CLI binary (`trivia`), web server, MCP server
apps/cli/www/    — React + TypeScript web UI (embedded at build time)
```

SQLite with [sqlite-vec](https://github.com/asg017/sqlite-vec) for vector search. Embeddings via [fastembed](https://github.com/Anush008/fastembed-rs) (AllMiniLM-L6-V2).

## Environment Variables

- `TRIVIA_DB` — database path (overrides config and default)
- `CLAUDE_PLUGIN_ROOT` — plugin root for config discovery

## License

MIT
