use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::BufWriter;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ql-engine")]
#[command(about = "QuickQL JSON query engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Stream {
        #[arg(long)]
        query: PathBuf,
        #[arg(long, default_value_t = quickql_core::DEFAULT_STREAM_BATCH_SIZE)]
        batch_size: usize,
    },
    Fields {
        #[arg(long)]
        query: PathBuf,
        #[arg(long, default_value_t = 100)]
        max_rows: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Stream { query, batch_size } => {
            let stdout = std::io::stdout();
            let lock = stdout.lock();
            let mut writer = BufWriter::new(lock);
            quickql_core::stream_query_jsonl(&query, &mut writer, batch_size)?;
        }
        Command::Fields { query, max_rows } => {
            let query_text = std::fs::read_to_string(&query)?;
            if let Some(source_path) = quickql_core::source_path_for_query(&query, &query_text)? {
                let fields = quickql_core::fields_from_source_sample(&source_path, max_rows)?;
                println!("{}", serde_json::to_string(&fields)?);
            } else {
                println!("[]");
            }
        }
    }
    Ok(())
}
