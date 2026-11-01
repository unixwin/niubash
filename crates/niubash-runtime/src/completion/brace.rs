//! Pure brace-compression port of GNU readline's `bracecomp.c`
//! (`hack_braces_completion` / `really_munge_braces`), used by
//! `complete-into-braces` (`M-{`, bashline.c:3795 `bash_brace_completion`).
//!
//! GNU completes filenames first, then rewrites the match list into one
//! csh-style brace expression, e.g. `foobar foobaz` becomes
//! `fooba{r,z}`. Every inserted substring is passed through
//! `sh_backslash_quote` (lib/sh/shquote.c:258) so the shell expands the
//! result to exactly the original filenames; the structural braces are
//! deliberately left unquoted.

/// Compress NAMES into one GNU-compatible brace expression.
///
/// NAMES are the raw filename matches (no shell quoting). They are sorted
/// and deduplicated like readline's match list, then compressed. An empty
/// list has no expression (`None`); a single name is returned quoted.
pub fn compress_braces(names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let mut sorted = names.to_vec();
    sorted.sort();
    sorted.dedup();
    Some(really_munge_braces(&sorted, 0, sorted.len(), 0))
}

/// `really_munge_braces` (bracecomp.c:68): find the next group of names
/// that share more than GCD_ZERO leading bytes and emit either the name or
/// `prefix{...}` for the group. The `}` that closes a nested group replaces
/// the trailing comma the recursive call leaves behind, exactly like GNU.
fn really_munge_braces(
    array: &[String],
    real_start: usize,
    real_end: usize,
    gcd_zero: usize,
) -> String {
    if real_start >= real_end {
        return array
            .first()
            .map(|name| sh_backslash_quote(&name[gcd_zero.min(name.len())..]))
            .unwrap_or_default();
    }

    let mut result = String::new();
    let mut leading_brace = false;
    let mut start = real_start;

    while start < real_end {
        let mut gcd = array[start].len();
        let mut next = start + 1;
        while next < real_end {
            let temp = string_gcd(&array[start], &array[next]);
            if temp <= gcd_zero {
                break;
            }
            gcd = temp;
            next += 1;
        }
        // C decrements its exclusive scan index once; `end` is the last
        // member of this group, inclusive.
        let end = next - 1;

        if gcd_zero == 0 && start == real_start && end != real_end - 1 {
            // Top level, and there is more than one group: the groups
            // themselves need a surrounding brace.
            result.push('{');
            leading_brace = true;
        }

        let subterm = if start == end {
            sh_backslash_quote(&array[start][gcd_zero..])
        } else {
            let mut quoted = sh_backslash_quote(&array[start][gcd_zero..gcd]);
            let mut inner = really_munge_braces(array, start, end + 1, gcd);
            // A nested call leaves its trailing comma in place; GNU
            // overwrites it with the closing brace.
            if inner.ends_with(',') {
                inner.pop();
            }
            inner.push('}');
            quoted.push('{');
            quoted.push_str(&inner);
            quoted
        };

        result.push_str(&subterm);
        result.push(',');
        start = end + 1;
    }

    if gcd_zero == 0 {
        // Drop the trailing comma; at top level a multi-group result also
        // closes the surrounding brace.
        result.pop();
        if leading_brace {
            result.push('}');
        }
    }
    result
}

/// `string_gcd` (bracecomp.c:53): the byte length of the greatest common
/// prefix of S1 and S2.
fn string_gcd(s1: &str, s2: &str) -> usize {
    s1.bytes()
        .zip(s2.bytes())
        .take_while(|(a, b)| a == b)
        .count()
}

/// `sh_backslash_quote(string, NULL, 0)` (lib/sh/shquote.c:258): quote the
/// default set of shell-special bytes, plus a leading `#`. Non-ASCII bytes
/// are copied through unchanged, matching GNU's multibyte path.
pub fn sh_backslash_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 4);
    for (index, ch) in value.char_indices() {
        if ch.is_ascii() && backslash_quote_byte(ch as u8) {
            quoted.push('\\');
        } else if ch == '#' && index == 0 {
            // `#` starts a comment only at the beginning of a word.
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted
}

/// The default `bstab` table from lib/sh/shquote.c:43.
fn backslash_quote_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'\t'
            | b'\n'
            | b' '
            | b'!'
            | b'"'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b','
            | b';'
            | b'<'
            | b'>'
            | b'?'
            | b'['
            | b'\\'
            | b']'
            | b'^'
            | b'`'
            | b'{'
            | b'|'
            | b'}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compress(names: &[&str]) -> String {
        compress_braces(
            &names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
        .expect("non-empty names compress")
    }

    #[test]
    fn brace_compression_matches_gnu_shapes() {
        // All names share a prefix: the prefix moves outside the braces.
        assert_eq!(compress(&["foobar", "foobaz"]), "fooba{r,z}");
        assert_eq!(compress(&["foo1", "foo2", "foo3"]), "foo{1,2,3}");
        // No common prefix: the whole list gets a surrounding brace.
        assert_eq!(compress(&["apple", "banana"]), "{apple,banana}");
        // Several groups at top level each keep their own prefix.
        assert_eq!(compress(&["file1", "file2", "other"]), "{file{1,2},other}");
        // A single match is just quoted, without braces.
        assert_eq!(compress(&["only"]), "only");
        assert_eq!(compress_braces(&[]), None);
    }

    #[test]
    fn brace_compression_backslash_quotes_special_bytes() {
        // Spaces, commas and literal braces are quoted; structural braces
        // are not.
        assert_eq!(compress(&["a b", "a c"]), "a\\ {b,c}");
        assert_eq!(compress(&["a,b", "a,c"]), "a\\,{b,c}");
        assert_eq!(compress(&["x{1", "x{2"]), "x\\{{1,2}");
        // A leading comment character is quoted like sh_backslash_quote.
        assert_eq!(compress(&["#a", "#b"]), "\\#{a,b}");
    }

    #[test]
    fn sh_backslash_quote_follows_the_default_bstab() {
        assert_eq!(sh_backslash_quote("plain-name.txt"), "plain-name.txt");
        assert_eq!(sh_backslash_quote("a b"), "a\\ b");
        assert_eq!(sh_backslash_quote("a{b}|c"), "a\\{b\\}\\|c");
        assert_eq!(sh_backslash_quote("a#b"), "a#b");
        assert_eq!(sh_backslash_quote("#tag"), "\\#tag");
        assert_eq!(sh_backslash_quote("a\\b"), "a\\\\b");
        assert_eq!(sh_backslash_quote("héllo"), "héllo");
    }
}
