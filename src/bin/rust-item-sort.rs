#![deny(warnings)]

use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use rust_item_sort::{ExecutionMode, item_sort_roots, item_sort_str};

#[derive(Parser, Debug, Clone)]
#[command(
	name = "rust-item-sort",
	about = "Sort/reorder Rust items in a source file in an opinionated way.",
	disable_version_flag = false
)]
struct Cli {
	/// Check whether sorting the file would change the file contents.
	#[arg(short = 'c', long = "check")]
	check: bool,

	/// Overwrite the file with the sorted contents.
	#[arg(short = 'w', long = "write")]
	overwrite: bool,

	/// One or more root Rust files to sort (module tree is followed like rustfmt).
	#[arg(value_name = "FILE", num_args = 1..)]
	paths: Vec<PathBuf>,
}

fn main() -> ExitCode {
	let cli = Cli::parse();

	match run(cli) {
		Ok(code) => code,
		Err(e) => {
			eprintln!("Error: {e}");
			ExitCode::FAILURE
		}
	}
}

fn run(cli: Cli) -> Result<ExitCode, String> {
	if cli.check {
		let changed = item_sort_roots(cli.paths.clone(), ExecutionMode::Check)?;
		return Ok(if changed.is_empty() {
			ExitCode::SUCCESS
		} else {
			ExitCode::FAILURE
		});
	}

	if cli.overwrite {
		item_sort_roots(cli.paths, ExecutionMode::Write)?;
		Ok(ExitCode::SUCCESS)
	} else {
		// Keep the original behavior: print the sorted output for a single file.
		if cli.paths.len() != 1 {
			return Err(
				"when not using --write/--check, exactly one FILE must be provided".to_owned(),
			);
		}
		let path = &cli.paths[0];
		let text = std::fs::read_to_string(path)
			.map_err(|e| format!("unable to read file '{}': {e}", path.display()))?;
		let out = item_sort_str(&text)?;
		println!("{out}");
		Ok(ExitCode::SUCCESS)
	}
}
