use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use trivia_core::{Embedder, MemoryStore};

mod www;

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
    /// Export all memories to a directory as markdown files
    Export {
        /// Target directory
        directory: String,
    },
    /// Import memories from a directory of markdown files
    Import {
        /// Source directory
        directory: String,
    },
    /// Start web UI server
    Www {
        /// Port to listen on
        #[arg(long, short, default_value_t = 3000)]
        port: u16,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let store = MemoryStore::new(&db_path())?;
    let embedder = Embedder::new()?;

    match cli.command {
        Command::Memorize {
            mnemonic,
            content,
            tag,
        } => {
            let embedding = embedder.embed(&mnemonic)?;
            store.memorize(&mnemonic, &content, &tag, &embedding)?;
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
            let memories = store.recall(&embedding, limit, tags)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&memories)?);
            } else if memories.is_empty() {
                println!("No memories found.");
            } else {
                for (i, mem) in memories.iter().enumerate() {
                    println!(
                        "{}. [{}] (score: {:.4}, distance: {:.4}, recalled: {} times)\n{}",
                        i + 1,
                        mem.mnemonic,
                        mem.score,
                        mem.distance,
                        mem.recall_count,
                        mem.content,
                    );
                    if !mem.tags.is_empty() {
                        println!("   tags: {}", mem.tags.join(", "));
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
        Command::Export { directory } => {
            let dir = std::path::Path::new(&directory);
            store.export(dir)?;
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
        Command::Www { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(www::serve(store, embedder, port))?;
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
    }

    Ok(())
}
