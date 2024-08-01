use std::{cmp::Ordering, convert::Infallible, ops::Range, path::PathBuf, process::ExitCode};

use pico_args::Arguments;
use tree_sitter::{Node, Parser};

const CLI_HELP: &str = r#"USAGE
  $ rust-organizer [-c] [-w] FILE

ARGUMENTS
  FILE    File name of the Rust source file to reorganize.

FLAGS
  -c, --check            Check whether reorganizing the file would change the file contents.
  -w, --write            Overwrite the file with the reorganized contents.
"#;

type ByteRange = Range<usize>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Cli {
	check: bool,
	overwrite: bool,
	path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Item<'a> {
	InnerDoc(ByteRange),
	Mod {
		name: &'a str,
		is_declaration: bool,
		content: ByteRange,
	},
	Use(ByteRange),
	Const {
		name: &'a str,
		content: ByteRange,
	},
	Type {
		name: &'a str,
		content: ByteRange,
	},
	Func {
		name: &'a str,
		content: ByteRange,
	},
	Impl {
		name: TypeIdent<'a>,
		trt: Option<&'a str>,
		content: ByteRange,
	},
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TypeIdent<'a> {
	name: &'a str,
	generics: Option<&'a str>,
}

fn main() -> ExitCode {
	// Parse commandline arguments
	let mut args = Arguments::from_env();
	if args.contains(["-h", "--help"]) {
		print!("{}", CLI_HELP);
		return ExitCode::SUCCESS;
	}
	let cli: Cli = match args.try_into() {
		Ok(cli) => cli,
		Err(e) => {
			eprintln!("Error: {}", e);
			return ExitCode::FAILURE;
		}
	};
	// Run the main program
	match cli.run() {
		Ok(code) => code,
		Err(e) => {
			eprintln!("Error: {}", e);
			ExitCode::FAILURE
		}
	}
}

impl Cli {
	fn run(&self) -> Result<ExitCode, String> {
		let mut parser = Parser::new();
		parser
			.set_language(&tree_sitter_rust::language())
			.expect("Error loading Rust grammar");

		let text = std::fs::read_to_string(&self.path)
			.map_err(|e| format!("unable to read file: {}", e))?;

		let Some(tree) = parser.parse(&text, None) else {
			return Err("unable to parse file".to_owned());
		};

		let mut items = Vec::new();

		let mut cursor = tree.walk();
		assert_eq!(cursor.node().kind(), "source_file");
		assert!(!cursor.goto_next_sibling());
		cursor.goto_first_child();
		let mut start = None;
		loop {
			let node = cursor.node();
			// println!("{} : {}\n\n", node.kind(), node.to_sexp());
			if let Some(item) = Item::maybe_item(&text, node, start) {
				let last = items.last();
				let inbetween =
					last.map(|(_, i): &(_, Item)| i.end_byte()).unwrap_or(0)..item.start_byte();
				debug_assert!(text[inbetween.clone()].trim().is_empty());
				let newline_before = text[inbetween].contains("\n\n");
				items.push((newline_before, item));
				start = None;
			} else if start.is_none() {
				start = Some(node.start_byte());
			}
			if !cursor.goto_next_sibling() {
				break;
			}
		}

		let mut is_sorted = true;
		for window in items.windows(2) {
			if window[0] > window[1] {
				if self.check {
					eprintln!(
						"Expected \n\"\"\"\n{}\n\"\"\"\n before \n\"\"\"\n{}\n\"\"\"",
						window[1].1.content(&text),
						window[0].1.content(&text)
					);
					return Ok(ExitCode::FAILURE);
				}
				is_sorted = false;
				break;
			}
		}
		if self.check || (self.overwrite && is_sorted) {
			return Ok(ExitCode::SUCCESS);
		}

		// Sort items by their order in the file
		items.sort_by(|a, b| a.1.cmp(&b.1));

		println!("{:?}", items);

		if self.overwrite {
			todo!()
		}

		let mut last = None;
		for (newline, item) in items {
			if newline || last != Some(item.item_order()) {
				println!();
			}
			println!("{}", item.content(&text));
			last = Some(item.item_order());
		}

		Ok(ExitCode::SUCCESS)
	}
}

impl TryFrom<Arguments> for Cli {
	type Error = String;

	fn try_from(mut args: Arguments) -> Result<Self, Self::Error> {
		let cli = Cli {
			check: args.contains(["-c", "--check"]),
			overwrite: args.contains(["-w", "--write"]),
			path: args
				.free_from_os_str::<_, Infallible>(|s| Ok(PathBuf::from(s)))
				.unwrap(),
		};

		let remaining = args.finish();
		match remaining.len() {
			0 => Ok(()),
			1 => Err(format!(
				"unexpected argument: '{}'",
				remaining[0].to_string_lossy()
			)),
			_ => Err(format!(
				"unexpected arguments: {}",
				remaining
					.into_iter()
					.map(|s| format!("'{}'", s.to_string_lossy()))
					.collect::<Vec<_>>()
					.join(", ")
			)),
		}?;
		Ok(cli)
	}
}

impl<'a> Item<'a> {
	fn byte_range(&self) -> ByteRange {
		match self {
			Item::InnerDoc(content)
			| Item::Mod { content, .. }
			| Item::Use(content)
			| Item::Const { content, .. }
			| Item::Type { content, .. }
			| Item::Func { content, .. }
			| Item::Impl { content, .. } => content.clone(),
		}
	}

	fn content(&self, text: &'a str) -> &'a str {
		match self {
			Item::InnerDoc(content)
			| Item::Mod { content, .. }
			| Item::Use(content)
			| Item::Const { content, .. }
			| Item::Type { content, .. }
			| Item::Func { content, .. }
			| Item::Impl { content, .. } => &text[content.clone()],
		}
	}

	fn end_byte(&self) -> usize {
		self.byte_range().end
	}

	fn item_order(&self) -> u8 {
		match self {
			Item::InnerDoc(_) => 0,
			Item::Mod {
				is_declaration: true,
				..
			} => 1,
			Item::Use(_) => 2,
			Item::Const { .. } => 3,
			Item::Type { .. } => 4,
			Item::Func { .. } => 5,
			Item::Impl { .. } => 6,
			Item::Mod {
				is_declaration: false,
				..
			} => 7,
		}
	}

	fn maybe_item(text: &'a str, node: Node<'a>, start: Option<usize>) -> Option<Self> {
		let get_field_str = |field_name| {
			node.child_by_field_name(field_name)
				.map(|n| n.utf8_text(text.as_bytes()).unwrap())
		};

		let start = start.unwrap_or(node.start_byte());
		match node.kind() {
			"attribute_item" => {
				// Ignore and add to the next item
				None
			}
			"block_comment" | "line_comment" => {
				let comment = node.utf8_text(text.as_bytes()).unwrap();
				if comment.starts_with("//!") || comment.starts_with("/*!") {
					// Doc comment for the file (ensure that it's at the top of the file).
					Some(Self::InnerDoc(start..node.end_byte()))
				} else {
					None // Move comment with the next item
				}
			}
			"const_item" => {
				let name = get_field_str("name").unwrap();
				let content = start..node.end_byte();
				Some(Self::Const { name, content })
			}
			"enum_item" | "struct_item" => {
				let name = get_field_str("name").unwrap();
				let content = start..node.end_byte();
				Some(Self::Type { name, content })
			}
			"function_item" => {
				let name = get_field_str("name").unwrap();
				let content = start..node.end_byte();
				Some(Self::Func { name, content })
			}
			"impl_item" => {
				let name = TypeIdent::from_node(text, node.child_by_field_name("type").unwrap());
				let trt = get_field_str("trait");
				let content = start..node.end_byte();
				Some(Self::Impl { name, trt, content })
			}
			"mod_item" => {
				let name = get_field_str("name").unwrap();
				let is_declaration = node.child_by_field_name("body").is_none();
				let content = start..node.end_byte();
				Some(Self::Mod {
					name,
					is_declaration,
					content,
				})
			}
			"use_declaration" => Some(Self::Use(start..node.end_byte())),
			_ => panic!("unexpected node kind: {}", node.kind()),
		}
	}

	fn start_byte(&self) -> usize {
		self.byte_range().start
	}
}

impl Ord for Item<'_> {
	fn cmp(&self, other: &Self) -> Ordering {
		let self_order = self.item_order();
		let other_order = other.item_order();
		if self_order != other_order {
			return self_order.cmp(&other_order);
		}
		match (self, other) {
			(Item::InnerDoc(_), Item::InnerDoc(_)) => Ordering::Equal,
			(Item::Const { name: a, .. }, Item::Const { name: b, .. })
			| (Item::Mod { name: a, .. }, Item::Mod { name: b, .. })
			| (Item::Type { name: a, .. }, Item::Type { name: b, .. })
			| (Item::Func { name: a, .. }, Item::Func { name: b, .. }) => a.cmp(b),
			(Item::Use(_), Item::Use(_)) => Ordering::Equal,
			(
				Item::Impl {
					name: a, trt: t_a, ..
				},
				Item::Impl {
					name: b, trt: t_b, ..
				},
			) => {
				let name_order = a.name.cmp(b.name);
				if name_order == Ordering::Equal {
					let trt_order = t_a.unwrap_or("").cmp(t_b.unwrap_or(""));
					if trt_order == Ordering::Equal {
						a.generics.unwrap_or("").cmp(&b.generics.unwrap_or(""))
					} else {
						trt_order
					}
				} else {
					name_order
				}
			}
			_ => unreachable!(),
		}
	}
}

impl PartialOrd for Item<'_> {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<'a> TypeIdent<'a> {
	fn from_node(text: &'a str, node: Node<'a>) -> Self {
		let get_field_str = |field_name| {
			node.child_by_field_name(field_name)
				.map(|n| n.utf8_text(text.as_bytes()).unwrap())
		};

		match node.kind() {
			"type_identifier" => Self {
				name: node.utf8_text(text.as_bytes()).unwrap(),
				generics: None,
			},
			"generic_type" => {
				let name = get_field_str("type").unwrap();
				let generics = get_field_str("type_arguments");
				debug_assert!(generics.is_some());
				Self { name, generics }
			}
			_ => panic!("invalid type identifier node: {}", node.kind()),
		}
	}
}
