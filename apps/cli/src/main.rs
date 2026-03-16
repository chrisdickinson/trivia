use std::collections::HashSet;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use trivia_core::{Embedder, MemoryStore, TriviaConfig};

use trivia_cli::{acl, mcp, www};

#[derive(Parser)]
#[command(name = "trivia", about = "Semantic memory store")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Store a fact or context for later recall
    Memorize {
        /// Short identifier (file path, concept, phrase)
        mnemonic: String,
        /// The fact or context to remember
        content: String,
        /// Categorization tags
        #[arg(long, short)]
        tag: Vec<String>,
    },
    /// Retrieve memories by semantic similarity
    Recall {
        /// Natural language search query
        query: String,
        /// Maximum number of results
        #[arg(long, short, default_value_t = 5)]
        limit: usize,
        /// Filter by tag
        #[arg(long, short)]
        tag: Vec<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Create a link between two memories
    Link {
        /// Mnemonic of the source memory
        source: String,
        /// Mnemonic of the target memory
        target: String,
        /// Type of link: related, supersedes, derived_from
        #[arg(long, short = 't', default_value = "related")]
        link_type: String,
    },
    /// Show all links for a memory
    Links {
        /// Mnemonic to show links for
        mnemonic: String,
    },
    /// Merge two memories: keep absorbs discard
    Merge {
        /// Mnemonic of the memory to keep
        keep: String,
        /// Mnemonic of the memory to absorb and delete
        discard: String,
    },
    /// Rate a memory as useful or not useful
    Rate {
        /// Mnemonic of the memory to rate
        mnemonic: String,
        /// Mark as useful
        #[arg(long, group = "rating")]
        useful: bool,
        /// Mark as not useful
        #[arg(long, group = "rating")]
        not_useful: bool,
    },
    /// Export memories to a directory as markdown files
    Export {
        /// Target directory
        directory: String,
        /// Only export memories with these tags
        #[arg(long, short)]
        tag: Vec<String>,
    },
    /// Import memories from a directory of markdown files
    Import {
        /// Source directory
        directory: String,
    },
    /// Start MCP server (stdin/stdout JSON-RPC)
    Mcp,
    /// Start web UI server
    Www {
        /// Tag-based ACL for shared MCP access (e.g. 'project:update,*:read')
        #[arg(long)]
        share: Option<String>,
    },
    /// List all unique tags with memory counts
    ListTags {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add an alias mnemonic to a memory
    AddMnemonic {
        /// Title (primary mnemonic) of the memory
        title: String,
        /// Alias text to add
        alias: String,
    },
    /// Remove an alias mnemonic from a memory
    RemoveMnemonic {
        /// Title (primary mnemonic) of the memory
        title: String,
        /// Alias text to remove
        alias: String,
    },
    /// Find and interactively merge similar memories
    Automerge {
        /// Max L2 distance to suggest as merge candidates
        #[arg(long, short, default_value_t = 0.25)]
        threshold: f64,
        /// Show candidates without prompting
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage users, providers, and identity links
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Add a user
    AddUser {
        /// Username
        username: String,
        /// ACL spec (e.g. '*:update', 'project:read,*:none')
        #[arg(long, default_value = "*:none")]
        acl: String,
    },
    /// Remove a user
    RemoveUser {
        /// Username
        username: String,
    },
    /// List all users
    ListUsers,
    /// Add an OAuth provider
    AddProvider {
        /// Provider name (e.g. 'github')
        name: String,
        /// Provider type
        #[arg(long = "type")]
        provider_type: String,
        /// OAuth client ID
        #[arg(long)]
        client_id: String,
        /// OAuth client secret
        #[arg(long)]
        client_secret: String,
    },
    /// Remove an OAuth provider
    RemoveProvider {
        /// Provider name
        name: String,
    },
    /// List all OAuth providers
    ListProviders,
    /// Link a user identity to an OAuth provider
    LinkIdentity {
        /// Username
        username: String,
        /// Provider name
        #[arg(long)]
        provider: String,
        /// Provider username (e.g. GitHub login)
        #[arg(long)]
        provider_username: String,
        /// Provider user ID (stable, numeric). If omitted, uses provider_username.
        #[arg(long)]
        provider_user_id: Option<String>,
    },
}

fn db_path(config: &TriviaConfig) -> PathBuf {
    if let Ok(path) = std::env::var("TRIVIA_DB") {
        PathBuf::from(path)
    } else if let Some(ref db) = config.database {
        PathBuf::from(db)
    } else {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".claude")
            .join("trivia.db")
    }
}

fn load_config() -> TriviaConfig {
    let start = std::env::var("CLAUDE_PLUGIN_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
    TriviaConfig::discover(&start)
        .map(|(c, _)| c)
        .unwrap_or_default()
}

fn main() -> Result<()> {
    let config = load_config();

    // Auto-detect: if stdin is not a TTY and no args, run MCP server
    if !io::stdin().is_terminal() && std::env::args().count() == 1 {
        let mut store = MemoryStore::new(&db_path(&config))?;
        if !config.recall.tags.is_empty() {
            store.set_boost_tags(config.recall.tags.clone());
        }
        let embedder = Embedder::new()?;
        let rt = tokio::runtime::Runtime::new()?;
        return rt.block_on(mcp::serve(store, embedder, config));
    }

    let cli = Cli::parse();
    let mut store = MemoryStore::new(&db_path(&config))?;
    if !config.recall.tags.is_empty() {
        store.set_boost_tags(config.recall.tags.clone());
    }
    let embedder = Embedder::new()?;

    match cli.command {
        Command::Memorize {
            mnemonic,
            content,
            tag,
        } => {
            let tags = TriviaConfig::merge_tags(&config.memorize.tags, &tag);
            let embedding = embedder.embed(&mnemonic)?;
            store.memorize(&mnemonic, &content, &tags, &embedding)?;
            eprintln!("Memorized: {mnemonic}");
        }
        Command::Recall {
            query,
            limit,
            tag,
            json,
        } => {
            let embedding = embedder.embed(&query)?;
            let tags = if tag.is_empty() {
                None
            } else {
                Some(tag.as_slice())
            };
            let memories = store.recall(&embedding, limit, tags, None, None)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&memories)?);
            } else if memories.is_empty() {
                println!("No memories found.");
            } else {
                for (i, mem) in memories.iter().enumerate() {
                    println!(
                        "{}. [{}] (score: {:.4})",
                        i + 1,
                        mem.mnemonic,
                        mem.score,
                    );
                    println!(
                        "   created: {} | updated: {} | recalled: {} times",
                        mem.created_at.format("%Y-%m-%dT%H:%M:%SZ"),
                        mem.updated_at.format("%Y-%m-%dT%H:%M:%SZ"),
                        mem.recall_count,
                    );
                    if !mem.tags.is_empty() {
                        println!("   tags: {}", mem.tags.join(", "));
                    }
                    if mem.mnemonics.len() > 1 {
                        let aliases: Vec<&str> = mem.mnemonics.iter()
                            .filter(|m| m.as_str() != mem.mnemonic)
                            .map(|m| m.as_str())
                            .collect();
                        if !aliases.is_empty() {
                            println!("   aliases: {}", aliases.join(", "));
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
                        println!("   links: {}", link_strs.join(", "));
                    }
                    println!();
                    println!("{}", mem.content);
                    println!();
                }
            }
        }
        Command::Rate {
            mnemonic,
            useful,
            not_useful,
        } => {
            if !useful && !not_useful {
                anyhow::bail!("specify --useful or --not-useful");
            }
            store.rate(&mnemonic, useful)?;
            let label = if useful { "useful" } else { "not useful" };
            eprintln!("Rated {mnemonic} as {label}");
        }
        Command::Link {
            source,
            target,
            link_type,
        } => {
            store.link(&source, &target, &link_type)?;
            println!("Linked: {} --[{}]--> {}", source, link_type, target);
        }
        Command::Merge { keep, discard } => {
            let embedding = embedder.embed(&keep)?;
            store.merge(&keep, &discard, &embedding)?;
            eprintln!("Merged: {keep} absorbed {discard}");
        }
        Command::Links { mnemonic } => {
            let links = store.get_links(&mnemonic)?;
            if links.is_empty() {
                println!("No links found for: {mnemonic}");
            } else {
                for link in &links {
                    println!(
                        "{} --[{}]--> {}",
                        link.source_mnemonic, link.link_type, link.target_mnemonic
                    );
                }
            }
        }
        Command::Export { directory, tag } => {
            let dir = std::path::Path::new(&directory);
            let merged = TriviaConfig::merge_tags(&config.export.tags, &tag);
            let tags = if merged.is_empty() {
                None
            } else {
                Some(merged.as_slice())
            };
            store.export(dir, tags)?;
            eprintln!("Exported to: {directory}");
        }
        Command::Import { directory } => {
            let dir = std::path::Path::new(&directory);
            let result = store.import(dir, &embedder)?;
            eprintln!(
                "Imported: {} created, {} updated, {} unchanged",
                result.created, result.updated, result.unchanged
            );
        }
        Command::Mcp => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::serve(store, embedder, config))?;
        }
        Command::Www { share } => {
            let bind_addr = std::env::var("BIND_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:3000".to_string());
            let acl = match share {
                Some(spec) => acl::Acl::parse(&spec)?,
                None => acl::Acl::closed(),
            };
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(www::serve(store, embedder, &bind_addr, config, acl))?;
        }
        Command::ListTags { json } => {
            let tags = store.list_tags()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&tags)?);
            } else if tags.is_empty() {
                println!("No tags found.");
            } else {
                for t in &tags {
                    println!("{} ({} memories)", t.tag, t.count);
                }
            }
        }
        Command::AddMnemonic { title, alias } => {
            let embedding = embedder.embed(&alias)?;
            store.add_mnemonic(&title, &alias, &embedding)?;
            eprintln!("Added mnemonic alias \"{alias}\" to \"{title}\"");
        }
        Command::RemoveMnemonic { title, alias } => {
            store.remove_mnemonic(&title, &alias)?;
            eprintln!("Removed mnemonic alias \"{alias}\" from \"{title}\"");
        }
        Command::Automerge {
            threshold,
            dry_run,
        } => {
            // ANSI codes
            const BOLD: &str = "\x1b[1m";
            const DIM: &str = "\x1b[2m";
            const RESET: &str = "\x1b[0m";
            const GREEN: &str = "\x1b[32m";
            const RED: &str = "\x1b[31m";
            const YELLOW: &str = "\x1b[33m";
            const CYAN: &str = "\x1b[36m";

            let truncate = |s: &str, max: usize| -> String {
                if s.len() <= max {
                    s.to_string()
                } else {
                    format!("{}{DIM}...{RESET}", &s[..max])
                }
            };

            let summaries = store.list_all_summaries()?;
            let mut discarded: HashSet<String> = HashSet::new();
            let mut merged_count = 0;
            let stdin = io::stdin();

            for summary in &summaries {
                if discarded.contains(&summary.mnemonic) {
                    continue;
                }

                let content_embedding = embedder.embed(&summary.content)?;

                let mut exclude = discarded.clone();
                exclude.insert(summary.mnemonic.clone());

                let candidates =
                    store.find_merge_candidates(&content_embedding, threshold, &exclude, 1)?;

                let candidate = match candidates.first() {
                    Some(c) => c,
                    None => continue,
                };

                eprintln!(
                    "\n{DIM}───────────────────────────────────────{RESET} {YELLOW}d={:.4}{RESET}",
                    candidate.distance,
                );
                // Keep side
                eprintln!(
                    "  {GREEN}{BOLD}keep{RESET}    {BOLD}{}{RESET}",
                    summary.mnemonic,
                );
                eprintln!("          {}", truncate(&summary.content, 200));
                if !summary.tags.is_empty() {
                    eprintln!("          {DIM}tags: {}{RESET}", summary.tags.join(", "));
                }
                // Discard side
                eprintln!(
                    "  {RED}{BOLD}discard{RESET} {BOLD}{}{RESET}",
                    candidate.mnemonic,
                );
                eprintln!("          {}", truncate(&candidate.content, 200));
                if !candidate.tags.is_empty() {
                    eprintln!("          {DIM}tags: {}{RESET}", candidate.tags.join(", "));
                }

                if dry_run {
                    continue;
                }

                eprint!(
                    "\n  {CYAN}{BOLD}[y]{RESET} merge  {CYAN}{BOLD}[s]{RESET} swap & merge  {CYAN}{BOLD}[l]{RESET} link  {CYAN}{BOLD}[n]{RESET} skip  {CYAN}{BOLD}[q]{RESET} quit  "
                );
                io::stderr().flush()?;

                let mut input = String::new();
                stdin.lock().read_line(&mut input)?;
                let choice = input.trim().to_lowercase();

                match choice.as_str() {
                    "y" | "yes" => {
                        let emb = embedder.embed(&summary.mnemonic)?;
                        store.merge(&summary.mnemonic, &candidate.mnemonic, &emb)?;
                        discarded.insert(candidate.mnemonic.clone());
                        merged_count += 1;
                        eprintln!("  {GREEN}Merged: {BOLD}{}{RESET}{GREEN} absorbed {}{RESET}", summary.mnemonic, candidate.mnemonic);
                    }
                    "s" | "swap" => {
                        let emb = embedder.embed(&candidate.mnemonic)?;
                        store.merge(&candidate.mnemonic, &summary.mnemonic, &emb)?;
                        discarded.insert(summary.mnemonic.clone());
                        merged_count += 1;
                        eprintln!("  {GREEN}Merged: {BOLD}{}{RESET}{GREEN} absorbed {}{RESET}", candidate.mnemonic, summary.mnemonic);
                    }
                    "l" | "link" => {
                        store.link(&summary.mnemonic, &candidate.mnemonic, "related")?;
                        eprintln!("  Linked: {} \u{2194} {}", summary.mnemonic, candidate.mnemonic);
                    }
                    "q" | "quit" => {
                        eprintln!("  {DIM}Quitting.{RESET}");
                        break;
                    }
                    _ => {
                        eprintln!("  {DIM}Skipped.{RESET}");
                    }
                }
            }

            eprintln!("\n{BOLD}{merged_count}{RESET} memories merged.");
        }
        Command::Admin { command: admin_cmd } => {
            match admin_cmd {
                AdminCommand::AddUser { username, acl: acl_spec } => {
                    // Validate the ACL spec parses
                    acl::Acl::parse(&acl_spec)?;
                    let user = store.create_user(&username, &acl_spec)?;
                    eprintln!("Created user: {} (acl: {})", user.username, user.acl);
                }
                AdminCommand::RemoveUser { username } => {
                    if store.delete_user(&username)? {
                        eprintln!("Removed user: {username}");
                    } else {
                        eprintln!("User not found: {username}");
                    }
                }
                AdminCommand::ListUsers => {
                    let users = store.list_users()?;
                    if users.is_empty() {
                        println!("No users.");
                    } else {
                        for u in &users {
                            println!("{} (acl: {})", u.username, u.acl);
                        }
                    }
                }
                AdminCommand::AddProvider {
                    name,
                    provider_type,
                    client_id,
                    client_secret,
                } => {
                    let prov = store.create_provider(&name, &provider_type, &client_id, &client_secret)?;
                    eprintln!("Created provider: {} (type: {})", prov.name, prov.provider_type);
                }
                AdminCommand::RemoveProvider { name } => {
                    if store.delete_provider(&name)? {
                        eprintln!("Removed provider: {name}");
                    } else {
                        eprintln!("Provider not found: {name}");
                    }
                }
                AdminCommand::ListProviders => {
                    let providers = store.list_providers()?;
                    if providers.is_empty() {
                        println!("No providers.");
                    } else {
                        for p in &providers {
                            let status = if p.enabled { "enabled" } else { "disabled" };
                            println!("{} (type: {}, {})", p.name, p.provider_type, status);
                        }
                    }
                }
                AdminCommand::LinkIdentity {
                    username,
                    provider,
                    provider_username,
                    provider_user_id,
                } => {
                    let user = store.get_user_by_username(&username)?
                        .ok_or_else(|| anyhow::anyhow!("user not found: {username}"))?;
                    let prov = store.get_provider_by_name(&provider)?
                        .ok_or_else(|| anyhow::anyhow!("provider not found: {provider}"))?;
                    let puid = provider_user_id.as_deref().unwrap_or(&provider_username);
                    store.link_identity(user.id, prov.id, &provider_username, puid)?;
                    eprintln!(
                        "Linked {username} to {provider} as {provider_username} (id: {puid})"
                    );
                }
            }
        }
    }

    Ok(())
}
