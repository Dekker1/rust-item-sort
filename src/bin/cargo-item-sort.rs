#![deny(warnings)]

use std::{
	cmp::Ordering,
	collections::{BTreeMap, BTreeSet},
	env, fs,
	hash::{Hash, Hasher},
	io::{self, Write},
	path::{Path, PathBuf},
	process::Command,
};

use cargo_metadata::Edition;
use clap::{CommandFactory, Parser};
use rust_item_sort::{item_sort_roots, ExecutionMode};

const FAILURE: i32 = 1;

const MESSAGE_FORMATS: &str = if is_nightly() {
	"short|json|human"
} else {
	"short|human"
};

const SUCCESS: i32 = 0;

#[derive(Debug, PartialEq, Eq)]
pub enum CargoFmtStrategy {
	All,
	Some(Vec<String>),
	Root,
}

#[derive(Parser)]
#[command(
	disable_version_flag = true,
	bin_name = "cargo item-sort",
	about = "Sort/reorder Rust items in crate targets (opinionated ordering) and then format them using rustfmt."
)]
#[command(styles = clap_cargo::style::CLAP_STYLING)]
pub struct Opts {
	/// No output printed to stdout
	#[arg(short = 'q', long = "quiet")]
	quiet: bool,

	/// Use verbose output
	#[arg(short = 'v', long = "verbose")]
	verbose: bool,

	/// Print rustfmt version and exit
	#[arg(long = "version")]
	version: bool,

	/// Specify package to format
	#[arg(short = 'p', long = "package", value_name = "package", num_args = 1..)]
	packages: Vec<String>,

	/// Specify path to Cargo.toml
	#[arg(long = "manifest-path", value_name = "manifest-path")]
	manifest_path: Option<String>,

	#[arg(
		long = "message-format",
		value_name = "message-format",
		help = format!("Specify message-format: {MESSAGE_FORMATS}")
	)]
	message_format: Option<String>,

	/// Options passed to rustfmt
	///
	/// 'raw = true' to make `--` explicit.
	#[arg(id = "rustfmt_options", raw = true)]
	rustfmt_options: Vec<String>,

	/// Format all packages, and also their local path-based dependencies
	#[arg(long = "all")]
	format_all: bool,

	/// Run rustfmt in check mode (also runs item-sort in check mode)
	#[arg(long = "check")]
	check: bool,
}

/// Target uses a `path` field for equality and hashing.
#[derive(Debug)]
pub struct Target {
	/// A path to the main source file of the target.
	path: PathBuf,
	/// A kind of target (e.g., lib, bin, example, ...).
	kind: String,
	/// Rust edition for this target.
	edition: Edition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
	Verbose,
	Normal,
	Quiet,
}

fn add_targets(target_paths: &[cargo_metadata::Target], targets: &mut BTreeSet<Target>) {
	for target in target_paths {
		targets.insert(Target::from_target(target));
	}
}

fn convert_message_format_to_rustfmt_args(
	message_format: &str,
	rustfmt_args: &mut Vec<String>,
) -> Result<(), String> {
	let mut contains_emit_mode = false;
	let mut contains_check = false;
	let mut contains_list_files = false;
	for arg in rustfmt_args.iter() {
		if arg.starts_with("--emit") {
			contains_emit_mode = true;
		}
		if arg == "--check" {
			contains_check = true;
		}
		if arg == "-l" || arg == "--files-with-diff" {
			contains_list_files = true;
		}
	}
	match message_format {
		"short" => {
			if !contains_list_files {
				rustfmt_args.push(String::from("-l"));
			}
			Ok(())
		}
		"json" => {
			if !is_nightly() {
				return Err(String::from(
					"--message-format json is only supported in nightly builds",
				));
			}
			if contains_emit_mode {
				return Err(String::from(
					"cannot include --emit arg when --message-format is set to json",
				));
			}
			if contains_check {
				return Err(String::from(
					"cannot include --check arg when --message-format is set to json",
				));
			}
			rustfmt_args.push(String::from("--emit"));
			rustfmt_args.push(String::from("json"));
			Ok(())
		}
		"human" => Ok(()),
		_ => Err(format!(
			"invalid --message-format value: {message_format}. Allowed values are: {MESSAGE_FORMATS}"
		)),
	}
}

fn execute() -> i32 {
	// Drop extra subcommand argument provided by `cargo`.
	let mut found_subcmd = false;
	let args = env::args().filter(|x| {
		if found_subcmd {
			true
		} else {
			found_subcmd = x == "item-sort";
			x != "item-sort"
		}
	});

	let opts = Opts::parse_from(args);

	let verbosity = match (opts.verbose, opts.quiet) {
		(false, false) => Verbosity::Normal,
		(false, true) => Verbosity::Quiet,
		(true, false) => Verbosity::Verbose,
		(true, true) => {
			print_usage_to_stderr("quiet mode and verbose mode are not compatible");
			return FAILURE;
		}
	};

	if opts.version {
		return handle_command_status(get_rustfmt_info(&[String::from("--version")]));
	}
	if opts.rustfmt_options.iter().any(|s| {
		["--print-config", "-h", "--help", "-V", "--version"].contains(&s.as_str())
			|| s.starts_with("--help=")
			|| s.starts_with("--print-config=")
	}) {
		return handle_command_status(get_rustfmt_info(&opts.rustfmt_options));
	}

	let strategy = CargoFmtStrategy::from_opts(&opts);
	let mut rustfmt_args = opts.rustfmt_options;
	if opts.check {
		let check_flag = "--check";
		if !rustfmt_args.iter().any(|o| o == check_flag) {
			rustfmt_args.push(check_flag.to_owned());
		}
	}
	if let Some(message_format) = opts.message_format {
		if let Err(msg) = convert_message_format_to_rustfmt_args(&message_format, &mut rustfmt_args)
		{
			print_usage_to_stderr(&msg);
			return FAILURE;
		}
	}

	let manifest_path = if let Some(specified_manifest_path) = opts.manifest_path {
		if !specified_manifest_path.ends_with("Cargo.toml") {
			print_usage_to_stderr("the manifest-path must be a path to a Cargo.toml file");
			return FAILURE;
		}
		Some(PathBuf::from(specified_manifest_path))
	} else {
		None
	};

	handle_command_status(format_crate(
		verbosity,
		&strategy,
		rustfmt_args,
		manifest_path.as_deref(),
		opts.check,
	))
}

fn format_crate(
	verbosity: Verbosity,
	strategy: &CargoFmtStrategy,
	rustfmt_args: Vec<String>,
	manifest_path: Option<&Path>,
	check: bool,
) -> Result<i32, io::Error> {
	let targets = get_targets(strategy, manifest_path)?;

	let mode = if check {
		ExecutionMode::Check
	} else {
		ExecutionMode::Write
	};
	let roots = targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>();
	let changed =
		item_sort_roots(roots, mode).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

	if check {
		if !changed.is_empty() {
			if verbosity != Verbosity::Quiet {
				for path in &changed {
					eprintln!("would item-sort: {}", path.display());
				}
			}
			return Ok(FAILURE);
		}
	}

	run_rustfmt(&targets, &rustfmt_args, verbosity)
}

fn get_cargo_metadata(manifest_path: Option<&Path>) -> Result<cargo_metadata::Metadata, io::Error> {
	let mut cmd = cargo_metadata::MetadataCommand::new();
	cmd.no_deps();
	if let Some(manifest_path) = manifest_path {
		cmd.manifest_path(manifest_path);
	}
	cmd.other_options(vec![String::from("--offline")]);

	match cmd.exec() {
		Ok(metadata) => Ok(metadata),
		Err(_) => {
			cmd.other_options(vec![]);
			match cmd.exec() {
				Ok(metadata) => Ok(metadata),
				Err(error) => Err(io::Error::new(io::ErrorKind::Other, error.to_string())),
			}
		}
	}
}

fn get_rustfmt_info(args: &[String]) -> Result<i32, io::Error> {
	let mut command = rustfmt_command()
		.stdout(std::process::Stdio::inherit())
		.args(args)
		.spawn()
		.map_err(|e| match e.kind() {
			io::ErrorKind::NotFound => io::Error::new(
				io::ErrorKind::Other,
				"Could not run rustfmt, please make sure it is in your PATH.",
			),
			_ => e,
		})?;
	let result = command.wait()?;
	if result.success() {
		Ok(SUCCESS)
	} else {
		Ok(result.code().unwrap_or(SUCCESS))
	}
}

fn get_targets(
	strategy: &CargoFmtStrategy,
	manifest_path: Option<&Path>,
) -> Result<BTreeSet<Target>, io::Error> {
	let mut targets = BTreeSet::new();

	match *strategy {
		CargoFmtStrategy::Root => get_targets_root_only(manifest_path, &mut targets)?,
		CargoFmtStrategy::All => {
			get_targets_recursive(manifest_path, &mut targets, &mut BTreeSet::new())?
		}
		CargoFmtStrategy::Some(ref hitlist) => {
			get_targets_with_hitlist(manifest_path, hitlist, &mut targets)?
		}
	}

	if targets.is_empty() {
		Err(io::Error::new(
			io::ErrorKind::Other,
			"Failed to find targets".to_owned(),
		))
	} else {
		Ok(targets)
	}
}

fn get_targets_recursive(
	manifest_path: Option<&Path>,
	targets: &mut BTreeSet<Target>,
	visited: &mut BTreeSet<String>,
) -> Result<(), io::Error> {
	let metadata = get_cargo_metadata(manifest_path)?;
	for package in &metadata.packages {
		add_targets(&package.targets, targets);

		for dependency in &package.dependencies {
			if dependency.path.is_none() || visited.contains(&dependency.name) {
				continue;
			}

			let manifest_path = PathBuf::from(dependency.path.as_ref().unwrap()).join("Cargo.toml");
			if manifest_path.exists()
				&& !metadata
					.packages
					.iter()
					.any(|p| p.manifest_path.eq(&manifest_path))
			{
				visited.insert(dependency.name.to_owned());
				get_targets_recursive(Some(&manifest_path), targets, visited)?;
			}
		}
	}

	Ok(())
}

fn get_targets_root_only(
	manifest_path: Option<&Path>,
	targets: &mut BTreeSet<Target>,
) -> Result<(), io::Error> {
	let metadata = get_cargo_metadata(manifest_path)?;
	let workspace_root_path = PathBuf::from(&metadata.workspace_root).canonicalize()?;
	let (in_workspace_root, current_dir_manifest) = if let Some(target_manifest) = manifest_path {
		let manifest = target_manifest.canonicalize()?;
		(
			workspace_root_path.join("Cargo.toml") == manifest,
			manifest,
		)
	} else {
		let current_dir = env::current_dir()?.canonicalize()?;
		(
			workspace_root_path == current_dir,
			current_dir.join("Cargo.toml"),
		)
	};

	let package_targets = match metadata.packages.len() {
		1 => metadata.packages.into_iter().next().unwrap().targets,
		_ => metadata
			.packages
			.into_iter()
			.filter(|p| {
				in_workspace_root
					|| PathBuf::from(&p.manifest_path)
						.canonicalize()
						.unwrap_or_default()
						== current_dir_manifest
			})
			.flat_map(|p| p.targets)
			.collect(),
	};

	for target in package_targets {
		targets.insert(Target::from_target(&target));
	}

	Ok(())
}

fn get_targets_with_hitlist(
	manifest_path: Option<&Path>,
	hitlist: &[String],
	targets: &mut BTreeSet<Target>,
) -> Result<(), io::Error> {
	let metadata = get_cargo_metadata(manifest_path)?;
	let mut workspace_hitlist: BTreeSet<&str> =
		BTreeSet::from_iter(hitlist.iter().map(|s| s.as_str()));

	for package in metadata.packages {
		if workspace_hitlist.remove(package.name.as_ref()) {
			for target in package.targets {
				targets.insert(Target::from_target(&target));
			}
		}
	}

	if workspace_hitlist.is_empty() {
		Ok(())
	} else {
		let package = workspace_hitlist.iter().next().unwrap();
		Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("package `{package}` is not a member of the workspace"),
		))
	}
}

fn handle_command_status(status: Result<i32, io::Error>) -> i32 {
	match status {
		Err(e) => {
			print_usage_to_stderr(&e.to_string());
			FAILURE
		}
		Ok(status) => status,
	}
}

const fn is_nightly() -> bool {
	match option_env!("CFG_RELEASE_CHANNEL") {
		None => true,
		Some(c) => matches!(c.as_bytes(), b"nightly" | b"dev"),
	}
}

fn main() {
	let exit_status = execute();
	std::io::stdout().flush().unwrap();
	std::process::exit(exit_status);
}

fn print_usage_to_stderr(reason: &str) {
	eprintln!("{reason}");
	let app = Opts::command();
	let help = app.after_help("").render_help();
	eprintln!("{help}");
}

fn run_rustfmt(
	targets: &BTreeSet<Target>,
	fmt_args: &[String],
	verbosity: Verbosity,
) -> Result<i32, io::Error> {
	let by_edition = targets
		.iter()
		.inspect(|t| {
			if verbosity == Verbosity::Verbose {
				println!("[{} ({})] {:?}", t.kind, t.edition, t.path)
			}
		})
		.fold(BTreeMap::new(), |mut h, t| {
			h.entry(&t.edition).or_insert_with(Vec::new).push(&t.path);
			h
		});

	let mut status = vec![];
	for (edition, files) in by_edition {
		let stdout = if verbosity == Verbosity::Quiet {
			std::process::Stdio::null()
		} else {
			std::process::Stdio::inherit()
		};

		if verbosity == Verbosity::Verbose {
			print!("rustfmt");
			print!(" --edition {edition}");
			fmt_args.iter().for_each(|f| print!(" {f}"));
			files.iter().for_each(|f| print!(" {}", f.display()));
			println!();
		}

		let mut command = rustfmt_command()
			.stdout(stdout)
			.args(files)
			.args(["--edition", edition.as_str()])
			.args(fmt_args)
			.spawn()
			.map_err(|e| match e.kind() {
				io::ErrorKind::NotFound => io::Error::new(
					io::ErrorKind::Other,
					"Could not run rustfmt, please make sure it is in your PATH.",
				),
				_ => e,
			})?;

		status.push(command.wait()?);
	}

	Ok(status
		.iter()
		.filter_map(|s| if s.success() { None } else { s.code() })
		.next()
		.unwrap_or(SUCCESS))
}

fn rustfmt_command() -> Command {
	// Priority:
	// 1) $RUSTFMT
	// 2) rustfmt next to this executable (toolchain-style install)
	// 3) rustfmt from PATH
	if let Some(rustfmt) = env::var_os("RUSTFMT") {
		return Command::new(PathBuf::from(rustfmt));
	}

	let sibling = env::current_exe()
		.expect("current executable path invalid")
		.with_file_name(if cfg!(windows) {
			"rustfmt.exe"
		} else {
			"rustfmt"
		});
	if sibling.exists() {
		Command::new(sibling)
	} else {
		Command::new("rustfmt")
	}
}

impl CargoFmtStrategy {
	pub fn from_opts(opts: &Opts) -> CargoFmtStrategy {
		match (opts.format_all, opts.packages.is_empty()) {
			(false, true) => CargoFmtStrategy::Root,
			(true, _) => CargoFmtStrategy::All,
			(false, false) => CargoFmtStrategy::Some(opts.packages.clone()),
		}
	}
}

impl Target {
	pub fn from_target(target: &cargo_metadata::Target) -> Self {
		let path = PathBuf::from(&target.src_path);
		let canonicalized = fs::canonicalize(&path).unwrap_or(path);

		Target {
			path: canonicalized,
			kind: target.kind[0].to_string(),
			edition: target.edition,
		}
	}
}

impl Eq for Target {}

impl Hash for Target {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.path.hash(state);
	}
}

impl Ord for Target {
	fn cmp(&self, other: &Target) -> Ordering {
		self.path.cmp(&other.path)
	}
}

impl PartialEq for Target {
	fn eq(&self, other: &Target) -> bool {
		self.path == other.path
	}
}

impl PartialOrd for Target {
	fn partial_cmp(&self, other: &Target) -> Option<Ordering> {
		Some(self.path.cmp(&other.path))
	}
}
