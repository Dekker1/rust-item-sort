//! Opinionated item sorter for Rust source files.
//!
//! ## What it does
//!
//! This crate provides:
//!
//! - A *single-file* transformation [`item_sort_str`] / [`item_sort_file`] that:
//!   1. parses a Rust file with tree-sitter,
//!   2. extracts top-level items into an internal representation,
//!   3. sorts them in a fixed order (and by name within that order),
//!   4. re-renders the file mostly by splicing original source text.
//! - A rustfmt-like *module traversal* [`item_sort_roots`] which, given one or more crate/target root
//!   files, follows out-of-line module declarations (`mod foo;`) to discover module files and sorts
//!   them as well.
//!
//! The intended workflow is:
//!
//! - `cargo item-sort`: select target roots like `cargo fmt`, run item sorting on the module tree,
//!   then invoke `rustfmt`.
//!
//! ## Item extraction and comment handling
//!
//! The file is walked left-to-right and each relevant syntax node is converted into an internal
//! `Item` variant. Items store their original source slices (`Cow<str>`) so formatting is preserved
//! as much as possible.
//!
//! - Attributes and non-inner comments are generally attached to the *following* item.
//! - If such skipped content occurs at the end of a module/file (e.g. mdBook anchors like
//!   `// ANCHOR_END: ...` right before `}`), it is preserved as a special trailing item so it is not
//!   dropped and does not get reflowed.
//!
//! Blank line detection treats lines containing only whitespace as blank as well.
//!
//! ## Sorting rules
//!
//! Items are sorted primarily by a fixed group order:
//!
//! 0. inner docs / inner attributes
//! 1. macro definitions
//! 2. `mod foo;` declarations
//! 3. `use` declarations
//! 4. `const` / `static`
//! 5. types (`struct`/`enum`/`type`/...) and `trait`
//! 6. functions
//! 7. impl blocks
//! 8. macro invocations
//! 9. inline modules (`mod foo { ... }`)
//!
//! Within a group, most items are sorted lexicographically by name.
//!
//! Bodies of inline `mod`/`trait`/`impl` items are recursively sorted.
//!
//! ## Module traversal (`item_sort_roots`)
//!
//! `item_sort_roots` mirrors rustfmt’s “start from root files and follow `mod` declarations” model.
//! It resolves `mod foo;` to either `foo.rs` or `foo/mod.rs` (and honors `#[path = "..."]`).
//!
//! This is syntactic, best-effort traversal: it does not evaluate `cfg(...)`.
//!
//! ## Non-goals / limitations
//!
//! - Not a full Rust name resolver (purely syntactic traversal).
//! - Does not try to merge/rewrite `use` statements; rustfmt still formats after sorting.

mod sort;

use std::{
	borrow::Cow,
	cmp::Ordering,
	collections::{BTreeSet, HashSet, VecDeque},
	fmt::Display,
	path::{Path, PathBuf},
};

use tree_sitter::{Node, Parser};

use crate::sort::version_cmp;

/// Controls whether the sorter should only report changes (`Check`) or rewrite files (`Write`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
	Check,
	Write,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Item<'a> {
	InnerDoc(Cow<'a, str>),
	Macro {
		name: &'a str,
		content: Cow<'a, str>,
	},
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
	Trait {
		name: &'a str,
		content: SortableContent<'a>,
	},
	Impl {
		name: TypeIdent<'a>,
		trt: Option<&'a str>,
		content: SortableContent<'a>,
	},
	MacroInvocation(Cow<'a, str>),
	Mod {
		name: &'a str,
		content: SortableContent<'a>,
	},
	/// Trailing content that would otherwise be dropped (typically end-of-module comments).
	Trailing(Cow<'a, str>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Module<'a> {
	items: Vec<(bool, Item<'a>)>,
	is_block: bool,
	/// Original text between `{` and `}` for empty blocks, used to preserve `{\n}` vs `{}`.
	between_braces: &'a str,
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
	reference_type: Option<&'a str>,
}

/// Collect paths for out-of-line modules referenced from `file_path`.
///
/// This implements a simplified version of Rust's module file resolution.
fn collect_module_files(
	parser: &mut Parser,
	input: &str,
	file_path: &Path,
	is_root: bool,
) -> Vec<PathBuf> {
	let Some(tree) = parser.parse(input, None) else {
		return vec![];
	};
	let root = tree.root_node();
	if root.kind() != "source_file" {
		return vec![];
	}

	let parent = file_path.parent().unwrap_or_else(|| Path::new("."));
	let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
	// Rust module path rules:
	// - For crate roots (lib.rs/main.rs and any bin root), submodules are relative to the parent dir.
	// - For `mod.rs`, submodules are relative to that directory.
	// - For other module files `foo.rs`, submodules are relative to `parent/foo/`.
	let dir = if is_root || file_name == "mod.rs" {
		parent.to_path_buf()
	} else {
		let stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
		if stem.is_empty() {
			parent.to_path_buf()
		} else {
			parent.join(stem)
		}
	};

	let mut cursor = root.walk();
	if !cursor.goto_first_child() {
		return vec![];
	}

	let mut pending_path_attr: Option<String> = None;
	let mut out = Vec::new();

	loop {
		let node = cursor.node();
		match node.kind() {
			"attribute_item" => {
				let txt = node.utf8_text(input.as_bytes()).unwrap_or("");
				if let Some(p) = parse_path_attribute(txt) {
					pending_path_attr = Some(p);
				}
			}
			// Only follow out-of-line modules (no body)
			"mod_item" if node.child_by_field_name("body").is_none() => {
				let name = node
					.child_by_field_name("name")
					.and_then(|n| n.utf8_text(input.as_bytes()).ok());
				if let Some(name) = name {
					let candidate = if let Some(rel) = pending_path_attr.take() {
						dir.join(rel)
					} else {
						let a = dir.join(format!("{name}.rs"));
						let b = dir.join(name).join("mod.rs");
						if a.exists() { a } else { b }
					};
					out.push(candidate);
				}
			}
			_ => {
				pending_path_attr = None;
			}
		}

		if !cursor.goto_next_sibling() {
			break;
		}
	}

	out
}

/// True if the string contains a blank line, where the middle line may contain whitespace.
fn has_blank_line(s: &str) -> bool {
	let bytes = s.as_bytes();
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'\n' {
			let mut j = i + 1;
			while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r') {
				j += 1;
			}
			if j < bytes.len() && bytes[j] == b'\n' {
				return true;
			}
		}
		i += 1;
	}
	false
}

/// Sort a single Rust source file on disk.
///
/// Returns `Ok(changed)`.
///
/// - In `ExecutionMode::Check`, the file is not modified, and `changed` indicates whether the output
///   would differ.
/// - In `ExecutionMode::Write`, the file is rewritten only if it would change.
pub fn item_sort_file(path: &Path, mode: ExecutionMode) -> Result<bool, String> {
	let input = std::fs::read_to_string(path)
		.map_err(|e| format!("unable to read file '{}': {e}", path.display()))?;
	let output = item_sort_str(&input)?;
	let changed = output != input;

	match mode {
		ExecutionMode::Check => Ok(changed),
		ExecutionMode::Write => {
			if changed {
				std::fs::write(path, output)
					.map_err(|e| format!("unable to write file '{}': {e}", path.display()))?;
			}
			Ok(changed)
		}
	}
}

/// Recursively item-sorts starting from one or more root files, following Rust module declarations
/// (`mod foo;`) similarly to how rustfmt traverses a crate.
///
/// The traversal is syntactic and best-effort:
///
/// - Out-of-line modules (`mod foo;`) are resolved to `foo.rs` or `foo/mod.rs`.
/// - `#[path = "..."] mod foo;` is honored.
/// - `cfg(...)` is not evaluated; if a module file exists, it may be visited.
///
/// Returns the set of files that would change (in `Check`) / did change (in `Write`).
pub fn item_sort_roots<I>(roots: I, mode: ExecutionMode) -> Result<BTreeSet<PathBuf>, String>
where
	I: IntoIterator<Item = PathBuf>,
{
	let mut parser = Parser::new();
	parser
		.set_language(&tree_sitter_rust::LANGUAGE.into())
		.expect("Error loading Rust grammar");

	let initial_roots: Vec<PathBuf> = roots.into_iter().collect();
	let root_set: HashSet<PathBuf> = initial_roots
		.iter()
		.map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
		.collect();

	let mut queue: VecDeque<PathBuf> = initial_roots.into_iter().collect();
	let mut visited: HashSet<PathBuf> = HashSet::new();
	let mut changed: BTreeSet<PathBuf> = BTreeSet::new();

	while let Some(path) = queue.pop_front() {
		let path = std::fs::canonicalize(&path).unwrap_or(path);
		if !visited.insert(path.clone()) {
			continue;
		}
		if !path.exists() {
			continue;
		}

		let input = std::fs::read_to_string(&path)
			.map_err(|e| format!("unable to read file '{}': {e}", path.display()))?;

		// First, sort this file itself.
		let did_change = match mode {
			ExecutionMode::Check => item_sort_str(&input)? != input,
			ExecutionMode::Write => item_sort_file(&path, ExecutionMode::Write)?,
		};
		if did_change {
			changed.insert(path.clone());
		}

		// Re-read after potential write so we discover modules from the updated order.
		let input_after = if mode == ExecutionMode::Write {
			std::fs::read_to_string(&path)
				.map_err(|e| format!("unable to read file '{}': {e}", path.display()))?
		} else {
			input
		};

		let is_root = root_set.contains(&path);
		let module_files = collect_module_files(&mut parser, &input_after, &path, is_root);
		queue.extend(module_files);
	}

	Ok(changed)
}

/// Sort a single Rust source file provided as a string.
///
/// This is a *pure* transformation: it does not touch the filesystem.
pub fn item_sort_str(input: &str) -> Result<String, String> {
	let mut parser = Parser::new();
	parser
		.set_language(&tree_sitter_rust::LANGUAGE.into())
		.expect("Error loading Rust grammar");

	let Some(tree) = parser.parse(input, None) else {
		return Err("unable to parse file".to_owned());
	};

	let root = tree.root_node();
	assert_eq!(root.kind(), "source_file");
	let mut root = Module::from_node(input, root);
	root.sort();
	Ok(root.to_string())
}

/// Like `node.end_byte()`, but if the node's text ends with `\n`, return `end_byte - 1`.
///
/// This is used when computing inter-item whitespace slices so that we can still "see" a trailing
/// newline in the whitespace *between* items.
fn node_end_no_trailing_newline(text: &str, node: Node<'_>) -> usize {
	let node_text = node.utf8_text(text.as_bytes()).unwrap_or("");
	if node_text.ends_with('\n') {
		node.end_byte() - 1
	} else {
		node.end_byte()
	}
}

/// Parse a `#[path = "..."]` attribute.
///
/// This is intentionally tiny and only supports the exact form used for module path overrides.
fn parse_path_attribute(attr: &str) -> Option<String> {
	// Extremely small parser for: #[path = "foo.rs"] (whitespace tolerated)
	let s = attr.trim();
	if !s.starts_with("#[") || !s.contains("path") {
		return None;
	}
	let idx = s.find("path")?;
	let rest = &s[idx + 4..];
	let first_quote = rest.find('"')?;
	let rest2 = &rest[first_quote + 1..];
	let end_quote = rest2.find('"')?;
	Some(rest2[..end_quote].to_string())
}

impl<'a> Item<'a> {
	fn append_content(&mut self, text: &str) {
		match self {
			Item::Macro { content, .. }
			| Item::ModDecl { content, .. }
			| Item::Const { content, .. }
			| Item::Type { content, .. }
			| Item::Func { content, .. }
			| Item::InnerDoc(content)
			| Item::Use(content)
			| Item::MacroInvocation(content)
			| Item::Trailing(content) => {
				*content = Cow::Owned(format!("{}{}", content, text));
			}
			Item::Impl { .. } | Item::Mod { .. } | Item::Trait { .. } => {
				// Cannot add content to these items
			}
		}
	}

	fn item_order(&self) -> u8 {
		match self {
			Item::InnerDoc(_) => 0,
			Item::Macro { .. } => 1,
			Item::ModDecl { .. } => 2,
			Item::Use(_) => 3,
			Item::Const { .. } => 4,
			Item::Type { .. } => 5,
			Item::Trait { .. } => 5,
			Item::Func { .. } => 6,
			Item::Impl { .. } => 7,
			Item::MacroInvocation(_) => 8,
			Item::Mod { .. } => 9,
			Item::Trailing(_) => 255,
		}
	}

	fn maybe_item(text: &'a str, node: Node<'a>, start: Option<usize>) -> Option<Self> {
		let get_field_str = |field_name| {
			node.child_by_field_name(field_name)
				.map(|n| n.utf8_text(text.as_bytes()).unwrap())
		};

		let start = start.unwrap_or(node.start_byte());
		let end = if node.utf8_text(text.as_bytes()).unwrap().ends_with('\n') {
			node.end_byte() - 1
		} else {
			node.end_byte()
		};
		let content: Cow<'a, str> = Cow::Borrowed(&text[start..end]);
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
			"const_item" | "static_item" => {
				let name = get_field_str("name").unwrap();
				Some(Self::Const { name, content })
			}
			"associated_type" | "enum_item" | "struct_item" | "type_item" => {
				let name = get_field_str("name").unwrap();
				Some(Self::Type { name, content })
			}
			"function_item" | "function_signature_item" => {
				let name = get_field_str("name").unwrap();
				Some(Self::Func { name, content })
			}
			"trait_item" => {
				let name = get_field_str("name").unwrap();
				let content = SortableContent::within_node(text, node, Some(start), "body");
				Some(Self::Trait { name, content })
			}
			"impl_item" => {
				let name = TypeIdent::from_node(text, node.child_by_field_name("type").unwrap());
				let trt = get_field_str("trait");
				let content = SortableContent::within_node(text, node, Some(start), "body");
				Some(Self::Impl { name, trt, content })
			}
			"inner_attribute_item" => {
				// Should be at the top (treat like inner doc, to keep it in the chosen
				// order compared to the module documentation).
				Some(Self::InnerDoc(content))
			}
			"macro_definition" => {
				let name = get_field_str("name").unwrap();
				Some(Self::Macro { name, content })
			}
			"macro_invocation" => Some(Self::MacroInvocation(content)),
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
			_ => panic!(
				"unexpected node kind: {}\ncontent: {}",
				node.kind(),
				content
			),
		}
	}
}

impl Display for Item<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Item::InnerDoc(content)
			| Item::Macro { content, .. }
			| Item::MacroInvocation(content)
			| Item::ModDecl { content, .. }
			| Item::Use(content)
			| Item::Const { content, .. }
			| Item::Type { content, .. }
			| Item::Func { content, .. }
			| Item::Trailing(content) => write!(f, "{content}"),
			Item::Mod { content, .. }
			| Item::Impl { content, .. }
			| Item::Trait { content, .. } => write!(f, "{content}"),
		}
	}
}

impl Ord for Item<'_> {
	fn cmp(&self, other: &Self) -> Ordering {
		use Item::*;

		let self_order = self.item_order();
		let other_order = other.item_order();
		if self_order != other_order {
			return self_order.cmp(&other_order);
		}
		match (self, other) {
			(InnerDoc(_), InnerDoc(_)) => Ordering::Equal,
			(Const { name: a, .. }, Const { name: b, .. })
			| (Macro { name: a, .. }, Macro { name: b, .. })
			| (Mod { name: a, .. }, Mod { name: b, .. })
			| (ModDecl { name: a, .. }, ModDecl { name: b, .. })
			| (
				Type { name: a, .. } | Trait { name: a, .. },
				Type { name: b, .. } | Trait { name: b, .. },
			)
			| (Func { name: a, .. }, Func { name: b, .. }) => version_cmp(a, b),
			(Use(_), Use(_))
			| (MacroInvocation(_), MacroInvocation(_))
			| (Trailing(_), Trailing(_)) => Ordering::Equal,
			(
				Impl {
					name: a, trt: t_a, ..
				},
				Impl {
					name: b, trt: t_b, ..
				},
			) => {
				let name_order = version_cmp(a.name, b.name);
				if name_order == Ordering::Equal {
					let trt_order = version_cmp(t_a.unwrap_or(""), t_b.unwrap_or(""));
					if trt_order == Ordering::Equal {
						version_cmp(a.generics.unwrap_or(""), b.generics.unwrap_or("")).then_with(
							|| {
								version_cmp(
									a.reference_type.unwrap_or(""),
									b.reference_type.unwrap_or(""),
								)
							},
						)
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

		let mut items: Vec<(bool, Item)> = Vec::new();
		let mut start = None;
		let mut last = None;
		let mut is_block = false;
		let mut between_braces: &'a str = "";
		if cursor.node().kind() == "{" {
			is_block = true;
			last = Some(cursor.node().end_byte());
			cursor.goto_next_sibling();
		}

		// Returns the byte offset to use as content start for a node inside a block,
		// including the leading indentation (tabs/spaces after the last newline).
		let indent_start = |last_pos: usize, node_start: usize| -> usize {
			if !is_block {
				return node_start;
			}
			let between = &text[last_pos..node_start];
			if let Some(nl_pos) = between.rfind('\n') {
				last_pos + nl_pos + 1
			} else {
				node_start
			}
		};

		loop {
			if cursor.node().kind() == "}" {
				// If we were collecting skipped content (e.g. trailing comments), attach it to the last item.
				if let Some(start_byte) = start {
					let tail = &text[start_byte..cursor.node().start_byte()];
					// Preserve trailing comments (e.g. mdbook anchors) as their own item-like chunk
					// so they stay on their own line and are not reflowed by rustfmt.
					if !tail.trim().is_empty() {
						let newline_before = has_blank_line(tail);
						items.push((
							items.is_empty() || newline_before,
							Item::Trailing(Cow::Borrowed(tail)),
						));
					} else if let Some((_, it)) = items.last_mut() {
						it.append_content(tail);
					}
					start = None;
				}
				// For empty blocks, remember what was between `{` and `}` so we can
				// reproduce `{\n}` vs `{}` faithfully instead of always emitting `{}`.
				if items.is_empty() && is_block {
					let close_start = cursor.node().start_byte();
					let last_pos = last.unwrap_or(root.start_byte());
					let between = &text[last_pos..close_start];
					let indent_before_close = if let Some(nl_pos) = between.rfind('\n') {
						last_pos + nl_pos + 1
					} else {
						close_start
					};
					between_braces = &text[last_pos..indent_before_close];
				}
				assert!(!cursor.goto_next_sibling());
				break;
			}
			let node = cursor.node();
			let last_pos = last.unwrap_or(root.start_byte());
			let inbetween = &text[last_pos..start.unwrap_or(node.start_byte())];
			if node.kind() == "empty_statement" {
				if let Some((_, it)) = items.last_mut() {
					it.append_content(";");
				}
				debug_assert!(
					inbetween.trim().is_empty(),
					"unexpected skipped content: {:?}",
					inbetween
				);
				start = None;
				last = Some(node_end_no_trailing_newline(text, node));
			} else if let Some(item) = Item::maybe_item(
				text,
				node,
				start.or_else(|| Some(indent_start(last_pos, node.start_byte()))),
			) {
				debug_assert!(
					inbetween.trim().is_empty(),
					"unexpected skipped content: {:?}",
					inbetween
				);
				let newline_before = has_blank_line(inbetween);
				items.push((items.is_empty() || newline_before, item));
				start = None;
				last = Some(node_end_no_trailing_newline(text, node));
			} else if start.is_none() {
				start = Some(indent_start(last_pos, node.start_byte()));
			}
			if !cursor.goto_next_sibling() {
				break;
			}
		}

		// Preserve any remaining skipped content at EOF (e.g. trailing comments).
		if let Some(start) = start {
			let tail = &text[start..root.end_byte()];
			if !tail.trim().is_empty() {
				let newline_before = has_blank_line(tail);
				items.push((
					items.is_empty() || newline_before,
					Item::Trailing(Cow::Borrowed(tail)),
				));
			} else if let Some((_, it)) = items.last_mut() {
				it.append_content(tail);
			}
		}

		Self {
			items,
			is_block,
			between_braces,
		}
	}

	pub fn sort(&mut self) {
		for it in self.items.iter_mut() {
			match &mut it.1 {
				Item::Mod { content, .. }
				| Item::Impl { content, .. }
				| Item::Trait { content, .. } => content.sort(),
				_ => {}
			}
		}
		self.items.sort_by(|a, b| a.1.cmp(&b.1));
	}
}

impl Display for Module<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// Empty block: reproduce the original whitespace between `{` and `}` verbatim.
		if self.is_block && self.items.is_empty() {
			return write!(f, "{}", self.between_braces);
		}
		let mut last = None;
		for (newline, item) in &self.items {
			let is_trailing = matches!(item, Item::Trailing(_));
			let order = item.item_order();
			if last.is_none() {
				// First item: inside a block, emit exactly one newline to separate from `{`.
				// At the top level (source_file) no leading newline is needed.
				if self.is_block {
					writeln!(f)?;
				}
			} else if *newline || (!is_trailing && last != Some(order)) {
				writeln!(f)?;
			}
			// Trailing items (e.g. mdBook anchor comments) already end with `\n`; using
			// `write!` avoids adding a second newline that would grow on every write pass.
			if is_trailing {
				write!(f, "{}", item)?;
			} else {
				writeln!(f, "{}", item)?;
			}
			if !is_trailing {
				last = Some(order);
			}
		}
		Ok(())
	}
}

impl<'a> SortableContent<'a> {
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
		let open_end = cursor.node().end_byte();
		let before = Cow::Borrowed(&text[start..open_end]);

		cursor.goto_parent();
		cursor.goto_last_child();
		assert_eq!(cursor.node().kind(), "}");
		let close_start = cursor.node().start_byte();

		// Build the inner module first so we can inspect whether it has trailing content.
		let inner = Module::from_node(text, body);

		// `after` normally starts at the `}` token.  When there is no trailing content the
		// whitespace between the last item and `}` (typically `\n\t`) is not captured by any
		// item, so we extend `after` backwards to include the indentation before `}`.
		// When trailing content IS present it already includes that indentation, so we leave
		// `after` starting at `}` to avoid duplication.
		let has_trailing = inner
			.items
			.last()
			.is_some_and(|(_, it)| matches!(it, Item::Trailing(_)));
		let after_start = if !has_trailing {
			let between = &text[open_end..close_start];
			if let Some(nl_pos) = between.rfind('\n') {
				open_end + nl_pos + 1
			} else {
				close_start
			}
		} else {
			close_start
		};
		let after = Cow::Borrowed(&text[after_start..node.end_byte()]);

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
			"array_type" => {
				let inner = node.child_by_field_name("element").unwrap();
				let mut ty = TypeIdent::from_node(text, inner);
				let reference_str = &text[node.start_byte()..inner.start_byte()];
				ty.reference_type = Some(reference_str);
				ty
			}
			"generic_type" => {
				let name = get_field_str("type").unwrap();
				let generics = get_field_str("type_arguments");
				debug_assert!(generics.is_some());
				Self {
					name,
					generics,
					reference_type: None,
				}
			}
			"reference_type" => {
				let inner = node.child_by_field_name("type").unwrap();
				let mut ty = TypeIdent::from_node(text, inner);
				let reference_str = &text[node.start_byte()..inner.start_byte()];
				ty.reference_type = Some(reference_str);
				ty
			}
			"type_identifier" | "scoped_type_identifier" | "primitive_type" | "bounded_type" => {
				Self {
					name: node.utf8_text(text.as_bytes()).unwrap(),
					generics: None,
					reference_type: None,
				}
			}
			_ => panic!(
				"invalid type identifier node: {}, `{}`",
				node.kind(),
				node.utf8_text(text.as_bytes()).unwrap()
			),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn organize_str_basic() {
		let input = "const Z: i32 = 1;\nconst A: i32 = 2;\n";
		let out = item_sort_str(input).unwrap();
		assert!(out.find("const A").unwrap() < out.find("const Z").unwrap());
	}

	#[test]
	fn version_sort_numeric_names_in_items() {
		let input = "const u128: i32 = 1;\nconst u16: i32 = 2;\nconst u8: i32 = 3;\n";
		let out = item_sort_str(input).unwrap();
		let p8 = out.find("u8").unwrap();
		let p16 = out.find("u16").unwrap();
		let p128 = out.find("u128").unwrap();
		assert!(p8 < p16 && p16 < p128, "got:\n{out}");
	}

	#[test]
	fn version_sort_raw_identifiers_in_items() {
		// Functions named with raw identifiers should sort by their underlying name. With naive
		// string sorting `r#async` would come after `client`; under the 2024-edition rule it
		// comes first.
		let input = "\
fn client() {}
fn r#async() {}
fn result() {}
";
		let out = item_sort_str(input).unwrap();
		let p_async = out.find("fn r#async").expect("r#async survived parsing");
		let p_client = out.find("fn client").unwrap();
		let p_result = out.find("fn result").unwrap();
		assert!(
			p_async < p_client && p_client < p_result,
			"unexpected order:\n{out}"
		);
	}
}
