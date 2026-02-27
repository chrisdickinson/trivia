# Trivia — Semantic Memory

Trivia gives you persistent memory across sessions. Use it to remember project context, decisions, patterns, and feedback.

## When to Use

- **Recall before working.** At the start of a task, `recall` relevant context. This surfaces past decisions, architecture notes, and known issues.
- **Memorize discoveries.** When you learn something important about the project — a design pattern, a gotcha, a user preference — `memorize` it.
- **Rate after using.** After recalling a memory, `rate` it as useful or not. This improves future ranking.

## Tool Guide

### memorize

Store a fact. The **mnemonic** is a short identifier (file path, concept, phrase) — think of it as the title. The **content** is the detail.

Good mnemonics describe *what the memory is about*:
- `"project architecture"` — how the code is organized
- `"auth flow"` — authentication implementation details
- `"feedback on src/api/handler.rs"` — review notes for a specific file
- `"user preference: testing"` — how the user wants tests written

Bad mnemonics are too generic or duplicate the content:
- `"note"`, `"important"`, `"thing I learned"`

### recall

Search by meaning, not exact text. Ask for what you need:
- `"how is error handling done"` — finds relevant architectural memories
- `"what did we decide about the API"` — surfaces decision records

Use `tags` to narrow results to a specific area.

### link

Connect related memories. Types:
- `related` — general association
- `supersedes` — this memory replaces an older one
- `derived_from` — this memory builds on another

### merge

When two memories cover the same topic, merge them. The `keep` memory absorbs `discard`'s content, tags, and links.

## Tags

If `trivia.toml` defines `[memorize] tags`, those tags are automatically added to every memorize call. This keeps project-specific memories organized.

When memorizing, add extra tags for specificity: `--tag api`, `--tag auth`, `--tag bug`.

## Best Practices

1. Recall before starting implementation work
2. Memorize architectural decisions, not transient details
3. Use descriptive mnemonics — they're used for embedding similarity
4. Rate recalled memories to train the ranking system
5. Don't memorize information that's already in CLAUDE.md or project docs
