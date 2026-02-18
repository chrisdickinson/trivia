use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use trivia_core::{Embedder, MemoryStore};

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
                        "{}. [{}] (distance: {:.4})\n{}",
                        i + 1,
                        mem.mnemonic,
                        mem.distance,
                        mem.content,
                    );
                    if !mem.tags.is_empty() {
                        println!("   tags: {}", mem.tags.join(", "));
                    }
                    println!();
                }
            }
        }
    }

    Ok(())
}
