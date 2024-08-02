use std::{
	borrow::Cow, cmp::Ordering, convert::Infallible, fmt::Display, path::PathBuf, process::ExitCode,
};

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Cli {
	check: bool,
	overwrite: bool,
	path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Item<'a> {
	InnerDoc(Cow<'a, str>),
	ModDecl {
		name: &'a str,
		content: Cow<'a, str>,
	},
	Use(Cow<'a, str>),
	Const {
		name: &'a str,
		content: Cow<'a, str>,
	},
	Type {
		name: &'a str,
		content: Cow<'a, str>,
	},
	Func {
		name: &'a str,
		content: Cow<'a, str>,
	},
	Impl {
		name: TypeIdent<'a>,
		trt: Option<&'a str>,
		content: SortableContent<'a>,
	},
	Mod {
		name: &'a str,
		content: SortableContent<'a>,
	},
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Module<'a> {
	items: Vec<(bool, Item<'a>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SortableContent<'a> {
	before: Cow<'a, str>,
	inner: Module<'a>,
	after: Cow<'a, str>,
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

		let root = tree.root_node();
		assert_eq!(root.kind(), "source_file");
		let mut root = Module::from_node(&text, root);

		let is_sorted = root.is_sorted(self.check);
		if self.check {
			return if is_sorted {
				Ok(ExitCode::SUCCESS)
			} else {
				Ok(ExitCode::FAILURE)
			};
		}
		if self.overwrite && is_sorted {
			return Ok(ExitCode::SUCCESS);
		}

		root.sort();

		if self.overwrite {
			std::fs::write(&self.path, root.to_string())
				.map_err(|e| format!("unable to write file: {}", e))?;
		} else {
			println!("{}", root);
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
	fn item_order(&self) -> u8 {
		match self {
			Item::InnerDoc(_) => 0,
			Item::ModDecl { .. } => 1,
			Item::Use(_) => 2,
			Item::Const { .. } => 3,
			Item::Type { .. } => 4,
			Item::Func { .. } => 5,
			Item::Impl { .. } => 6,
			Item::Mod { .. } => 7,
		}
	}

	fn maybe_item(text: &'a str, node: Node<'a>, start: Option<usize>) -> Option<Self> {
		let get_field_str = |field_name| {
			node.child_by_field_name(field_name)
				.map(|n| n.utf8_text(text.as_bytes()).unwrap())
		};

		let start = start.unwrap_or(node.start_byte());
		let content: Cow<'a, str> = Cow::Borrowed(&text[start..node.end_byte()]);
		match node.kind() {
			"attribute_item" => {
				// Ignore and add to the next item
				None
			}
			"block_comment" | "line_comment" => {
				let comment = node.utf8_text(text.as_bytes()).unwrap();
				if comment.starts_with("//!") || comment.starts_with("/*!") {
					// Doc comment for the file (ensure that it's at the top of the file).
					Some(Self::InnerDoc(content))
				} else {
					None // Move comment with the next item
				}
			}
			"const_item" => {
				let name = get_field_str("name").unwrap();
				Some(Self::Const { name, content })
			}
			"enum_item" | "struct_item" => {
				let name = get_field_str("name").unwrap();
				Some(Self::Type { name, content })
			}
			"function_item" => {
				let name = get_field_str("name").unwrap();
				Some(Self::Func { name, content })
			}
			"impl_item" => {
				let name = TypeIdent::from_node(text, node.child_by_field_name("type").unwrap());
				let trt = get_field_str("trait");
				let content = SortableContent::within_node(text, node, Some(start), "body");
				Some(Self::Impl { name, trt, content })
			}
			"mod_item" => {
				let name = get_field_str("name").unwrap();
				if node.child_by_field_name("body").is_some() {
					let content = SortableContent::within_node(text, node, Some(start), "body");
					Some(Self::Mod { name, content })
				} else {
					Some(Self::ModDecl { name, content })
				}
			}
			"use_declaration" => Some(Self::Use(content)),
			_ => panic!("unexpected node kind: {}", node.kind()),
		}
	}
}

impl Display for Item<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Item::InnerDoc(content)
			| Item::ModDecl { content, .. }
			| Item::Use(content)
			| Item::Const { content, .. }
			| Item::Type { content, .. }
			| Item::Func { content, .. } => write!(f, "{content}"),
			Item::Mod { content, .. } | Item::Impl { content, .. } => {
				write!(f, "{content}")
			}
		}
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

impl<'a> Module<'a> {
	pub fn from_node(text: &'a str, root: Node<'a>) -> Self {
		assert!(matches!(root.kind(), "source_file" | "declaration_list"));
		let mut cursor = root.walk();
		cursor.goto_first_child();

		let mut items = Vec::new();
		let mut start = None;
		let mut last = None;
		if cursor.node().kind() == "{" {
			last = Some(cursor.node().end_byte());
			cursor.goto_next_sibling();
		}
		loop {
			let node = cursor.node();
			// println!("{} : {}\n\n", node.kind(), node.to_sexp());
			if let Some(item) = Item::maybe_item(&text, node, start) {
				let inbetween =
					&text[last.unwrap_or(root.start_byte())..start.unwrap_or(node.start_byte())];
				debug_assert!(
					inbetween.trim().is_empty(),
					"unexpected skipped content: {:?}",
					inbetween
				);
				let newline_before = inbetween.contains("\n\n");
				items.push((newline_before, item));
				start = None;
				last = Some(node.end_byte());
			} else if start.is_none() {
				start = Some(node.start_byte());
			}
			if !cursor.goto_next_sibling() {
				break;
			}
			if cursor.node().kind() == "}" {
				assert!(!cursor.goto_next_sibling());
				break;
			}
		}

		Self { items }
	}

	pub fn is_sorted(&self, print_diff: bool) -> bool {
		for it in &self.items {
			match &it.1 {
				Item::Mod { content, .. } | Item::Impl { content, .. } => {
					if !content.is_sorted(print_diff) {
						return false;
					}
				}
				_ => {}
			}
		}
		for window in self.items.windows(2) {
			if window[0] > window[1] {
				if print_diff {
					eprintln!(
						"Expected \n\"\"\"\n{}\n\"\"\"\n before \n\"\"\"\n{}\n\"\"\"",
						window[1].1, window[0].1
					);
					return false;
				}
			}
		}
		true
	}

	pub fn sort(&mut self) {
		for it in self.items.iter_mut() {
			match &mut it.1 {
				Item::Mod { content, .. } | Item::Impl { content, .. } => content.sort(),
				_ => {}
			}
		}
		self.items.sort_unstable_by(|a, b| a.1.cmp(&b.1));
	}
}

impl Display for Module<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let mut last = None;
		for (newline, item) in &self.items {
			if *newline || last != Some(item.item_order()) {
				writeln!(f)?;
			}
			writeln!(f, "{}", item)?;
			last = Some(item.item_order());
		}
		Ok(())
	}
}

impl<'a> SortableContent<'a> {
	fn is_sorted(&self, print_diff: bool) -> bool {
		self.inner.is_sorted(print_diff)
	}

	fn sort(&mut self) {
		self.inner.sort();
	}

	fn within_node(
		text: &'a str,
		node: Node<'a>,
		start: Option<usize>,
		child: &'static str,
	) -> Self {
		let start = start.unwrap_or(node.start_byte());
		let body = node.child_by_field_name(child).unwrap();

		let mut cursor = body.walk();
		cursor.goto_first_child();
		assert_eq!(cursor.node().kind(), "{");
		let before = Cow::Borrowed(&text[start..cursor.node().end_byte()]);

		cursor.goto_parent();
		cursor.goto_last_child();
		assert_eq!(cursor.node().kind(), "}");
		let after = Cow::Borrowed(&text[cursor.node().start_byte()..node.end_byte()]);

		let inner = Module::from_node(text, body);
		Self {
			before,
			inner,
			after,
		}
	}
}

impl Display for SortableContent<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}{}{}", self.before, self.inner, self.after)
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
