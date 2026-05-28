//! Identifier sort order used when ordering items within a group.
//!
//! Implements the Rust style guide's "version sorting" algorithm, with the 2024-edition rule for
//! raw identifiers (sort `r#ident` as `ident`).

use std::cmp::Ordering;

/// Compare two strings using the Rust style guide's "version sorting" algorithm.
///
/// See <https://doc.rust-lang.org/nightly/style-guide/index.html#sorting>.
///
/// Briefly:
/// - Strings are split into maximal-length chunks of either ASCII digits or non-digits.
/// - Numeric chunks compare by numeric value (ignoring leading zeros). If values are equal but the
///   number of leading zeros differs, the chunks are treated as equal but the earliest such
///   difference is remembered as a tie-breaker (more leading zeros sorts first).
/// - Non-numeric chunks compare character-by-character with two exceptions: `_` sorts immediately
///   after space but before any other character, and non-lowercase characters sort before
///   lowercase characters.
/// - A leading `r#` (raw identifier prefix) is stripped before comparison, matching rustfmt's
///   2024-edition behavior: <https://doc.rust-lang.org/edition-guide/rust-2024/rustfmt-raw-identifier-sorting.html>.
pub(crate) fn version_cmp(a: &str, b: &str) -> Ordering {
	fn char_key(c: char) -> (u8, char) {
		if c == ' ' {
			(0, c)
		} else if c == '_' {
			(1, c)
		} else if c.is_lowercase() {
			(3, c)
		} else {
			(2, c)
		}
	}

	fn compare_chars(a: char, b: char) -> Ordering {
		char_key(a).cmp(&char_key(b))
	}

	let a = a.strip_prefix("r#").unwrap_or(a);
	let b = b.strip_prefix("r#").unwrap_or(b);
	let a_bytes = a.as_bytes();
	let b_bytes = b.as_bytes();
	let mut i = 0;
	let mut j = 0;
	let mut leading_zero_tiebreak: Option<Ordering> = None;

	while i < a_bytes.len() && j < b_bytes.len() {
		let a_digit = a_bytes[i].is_ascii_digit();
		let b_digit = b_bytes[j].is_ascii_digit();
		if a_digit && b_digit {
			// Both sides are at the start of a digit run: compare numerically.
			let a_start = i;
			while i < a_bytes.len() && a_bytes[i].is_ascii_digit() {
				i += 1;
			}
			let b_start = j;
			while j < b_bytes.len() && b_bytes[j].is_ascii_digit() {
				j += 1;
			}
			let a_run = &a[a_start..i];
			let b_run = &b[b_start..j];
			let a_val = a_run.trim_start_matches('0');
			let b_val = b_run.trim_start_matches('0');
			let c = match a_val.len().cmp(&b_val.len()) {
				Ordering::Equal => a_val.cmp(b_val),
				other => other,
			};
			if c != Ordering::Equal {
				return c;
			}
			let a_lz = a_run.len() - a_val.len();
			let b_lz = b_run.len() - b_val.len();
			if a_lz != b_lz && leading_zero_tiebreak.is_none() {
				// More leading zeros sorts first (Less).
				leading_zero_tiebreak = Some(b_lz.cmp(&a_lz));
			}
		} else {
			// At least one side is non-digit: compare a single character on each side using the
			// character-key rules (this also handles the digit-vs-non-digit case).
			let ca = a[i..].chars().next().unwrap();
			let cb = b[j..].chars().next().unwrap();
			let c = compare_chars(ca, cb);
			if c != Ordering::Equal {
				return c;
			}
			i += ca.len_utf8();
			j += cb.len_utf8();
		}
	}
	match (i < a_bytes.len(), j < b_bytes.len()) {
		(false, false) => leading_zero_tiebreak.unwrap_or(Ordering::Equal),
		(false, true) => Ordering::Less,
		(true, false) => Ordering::Greater,
		(true, true) => unreachable!(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn matches_style_guide_example() {
		// The expected order from the Rust style guide.
		let expected = [
			"_ZYXW", "_abcd", "A2", "ABCD", "Z_YXW", "ZY_XW", "ZYXW", "ZYXW_", "a1", "abcd",
			"u_zzz", "u8", "u16", "u32", "u64", "u128", "u256", "ua", "usize", "uz", "v000", "v00",
			"v0", "v0s", "v00t", "v0u", "v001", "v01", "v1", "v009", "v09", "v9", "v010", "v10",
			"w005s09t", "w5s009t", "x64", "x86", "x86_32", "x86_64", "x86_128", "x87", "zyxw",
		];
		for w in expected.windows(2) {
			assert_eq!(
				version_cmp(w[0], w[1]),
				Ordering::Less,
				"expected {:?} < {:?}",
				w[0],
				w[1]
			);
		}

		// Sorting a shuffled copy should reproduce the expected order.
		let mut shuffled: Vec<&str> = expected.to_vec();
		shuffled.reverse();
		shuffled.sort_by(|a, b| version_cmp(a, b));
		assert_eq!(shuffled, expected);
	}

	#[test]
	fn raw_identifiers() {
		// `r#async` should sort by `async`, not by `r`.
		assert_eq!(version_cmp("r#async", "client"), Ordering::Less);
		assert_eq!(version_cmp("client", "r#async"), Ordering::Greater);
		assert_eq!(version_cmp("r#async", "result"), Ordering::Less);
		// Two raw identifiers sort by their underlying names.
		assert_eq!(version_cmp("r#type", "r#fn"), Ordering::Greater);
		// A raw identifier equal to its underlying form sorts as equal.
		assert_eq!(version_cmp("r#type", "type"), Ordering::Equal);
	}
}
