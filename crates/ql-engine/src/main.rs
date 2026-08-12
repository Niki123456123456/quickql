use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

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
        /// Prefer cached JSON files for nested QL sources.
        #[arg(long)]
        cache_folder: Option<PathBuf>,
    },
    /// Execute a query and write its rows to a JSON file beside the query.
    Write {
        #[arg(long)]
        query: PathBuf,
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
        Command::Stream {
            query,
            batch_size,
            cache_folder,
        } => {
            let stdout = std::io::stdout();
            let lock = stdout.lock();
            let mut writer = BufWriter::new(lock);
            if let Some(cache_folder) = cache_folder {
                quickql_core::stream_query_jsonl_with_cache_folder(
                    &query,
                    &mut writer,
                    batch_size,
                    &cache_folder,
                )?;
            } else {
                quickql_core::stream_query_jsonl(&query, &mut writer, batch_size)?;
            }
        }
        Command::Write { query } => {
            let stdout = std::io::stdout();
            let lock = stdout.lock();
            let mut writer = BufWriter::new(lock);
            write_query_to_json(&query, &mut writer)?;
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

fn json_output_path(query: &Path) -> PathBuf {
    query.with_extension("json")
}

fn write_query_to_json<W: Write>(query: &Path, progress_writer: &mut W) -> Result<PathBuf> {
    let output_path = json_output_path(query);
    writeln!(progress_writer, "{}", output_path.display())?;
    progress_writer.flush()?;

    let result = quickql_core::execute_query_with_progress(query, progress_writer)?;
    let file = std::fs::File::create(&output_path)
        .with_context(|| format!("Creating output file {}", output_path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, &result.rows)
        .with_context(|| format!("Writing output file {}", output_path.display()))?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn json_output_path_replaces_query_extension() {
        assert_eq!(
            json_output_path(Path::new("queries/ks_entries_basic_nomic2_post.ql")),
            PathBuf::from("queries/ks_entries_basic_nomic2_post.json")
        );
    }

    #[test]
    fn writes_query_rows_as_json_array() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ql-engine-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let query_path = temp_dir.join("example.ql");
        std::fs::write(&query_path, "SOURCE [{id: 1}, {id: 2}]\nMAP id").unwrap();

        let mut console = Vec::new();
        let output_path = write_query_to_json(&query_path, &mut console).unwrap();
        let output: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&output_path).unwrap()).unwrap();

        assert_eq!(output_path, temp_dir.join("example.json"));
        assert_eq!(output, serde_json::json!([{ "id": 1 }, { "id": 2 }]));

        let console = String::from_utf8(console).unwrap();
        let mut lines = console.lines();
        assert_eq!(lines.next(), Some(output_path.to_str().unwrap()));
        assert!(lines.all(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|message| message["type"] == "progress")
        }));
        std::fs::remove_dir_all(temp_dir).unwrap();
    }
}
