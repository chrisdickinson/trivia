# trivia

Semantic memory for Claude Code. Memorize facts, recall them by meaning, and let connections form automatically.

## Quick Start

```bash
# Install (from source for now)
cargo install --path apps/cli

# As a Claude Code plugin
claude plugin marketplace add chrisdickinson/trivia
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

# Optional: storage backend — "sqlite" (default) or "s3vectors"
# backend = "sqlite"
```

Config discovery walks up from CWD (or `CLAUDE_PLUGIN_ROOT`) to find the nearest `trivia.toml`. CLI flags are additive with config tags.

## Storage Backends

Retrieval sits behind a `MemoryBackend` trait: the backend does storage and the
initial KNN search, then a pure, backend-agnostic reranker applies the composite
score. Pick a backend with `--backend`, `backend =` in `trivia.toml`, or
`TRIVIA_BACKEND`.

- **`sqlite`** (default) — local [sqlite-vec](https://github.com/asg017/sqlite-vec)
  index. Zero-config, offline, supports full-text keyword boosting.
- **`s3vectors`** — [Amazon S3 Vectors](https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-vectors.html).
  Vectors and memory records live in an S3 vector index; recall runs
  `QueryVectors`. No local database needed for memory (the web server still keeps
  auth/sessions in local SQLite). No lexical/FTS boost. Ships in release binaries;
  build from source with `--features s3vectors`.

```toml
backend = "s3vectors"

[s3vectors]
bucket = "my-trivia-vectors"
index  = "memories"
region = "us-east-1"   # optional; falls back to the standard AWS chain
```

The bucket and index must already exist — provision them with Terraform's
[`aws_s3vectors_vector_bucket`](https://registry.terraform.io/providers/hashicorp/aws/latest/docs/resources/s3vectors_vector_bucket)
and a vector index of dimension **384**, distance metric **euclidean**, with
`record` configured as a **non-filterable** metadata key. Credentials come from
the standard AWS provider chain (env, profile, IMDS, etc.).

## Web UI

Start with `trivia www` and open `http://localhost:3000`. Features:

- Memory list with search and tag filtering
- Memory detail view with inline editing (including mnemonic rename)
- Link management
- Interactive merge
- Force-directed graph visualization

## Architecture

```
crates/core/         — MemoryStore, Embedder, config, export/import
crates/core/backend/ — MemoryBackend/AuthBackend traits + sqlite & s3vectors impls
apps/cli/            — CLI binary (`trivia`), web server, MCP server
apps/cli/www/        — React + TypeScript web UI (embedded at build time)
```

Storage is abstracted behind the `MemoryBackend` trait (storage, retrieval, and
initial KNN); reranking is a pure Rust step shared by every backend. `AuthBackend`
covers the web server's users/OAuth/sessions and is always SQLite. Default vector
search is SQLite with [sqlite-vec](https://github.com/asg017/sqlite-vec);
embeddings via [fastembed](https://github.com/Anush008/fastembed-rs)
(AllMiniLM-L6-V2). See [Storage Backends](#storage-backends).

## Environment Variables

- `TRIVIA_DB` — database path (overrides config and default)
- `TRIVIA_BACKEND` — storage backend: `sqlite` or `s3vectors`
- `TRIVIA_S3_BUCKET` / `TRIVIA_S3_INDEX` / `TRIVIA_S3_REGION` — S3 Vectors settings
- `CLAUDE_PLUGIN_ROOT` — plugin root for config discovery

## License

MIT
