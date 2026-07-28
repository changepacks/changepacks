//! Byte-level JSON span scanner used to rewrite changepack logs in place.
//!
//! `serde_json` round-trips would reflow a hand-formatted changepack log, so
//! [`remove_applied_change_spans`] never re-serializes: it locates the byte
//! spans of the applied `changes` members in the ORIGINAL text and splices
//! them out. Everything the caller does not touch — key order, indentation,
//! spacing, trailing newline — survives byte-for-byte.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

struct JsonObjectMember {
    prefix_start: usize,
    key: String,
    value_start: usize,
    value_end: usize,
    comma: Option<usize>,
}

fn skip_json_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'\t' | b'\r' | b'\n') {
        cursor += 1;
    }
    cursor
}

fn scan_json_string_end(bytes: &[u8], start: usize) -> Result<usize> {
    if bytes.get(start) != Some(&b'"') {
        bail!("expected JSON string at byte {start}");
    }

    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            b'"' => return Ok(cursor + 1),
            _ => cursor += 1,
        }
    }
    bail!("unterminated JSON string at byte {start}")
}

fn scan_json_value_end(bytes: &[u8], start: usize) -> Result<usize> {
    let start = skip_json_whitespace(bytes, start);
    match bytes.get(start) {
        Some(b'"') => scan_json_string_end(bytes, start),
        Some(b'{') | Some(b'[') => {
            let mut closers = vec![if bytes[start] == b'{' { b'}' } else { b']' }];
            let mut cursor = start + 1;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'"' => cursor = scan_json_string_end(bytes, cursor)?,
                    b'{' => {
                        closers.push(b'}');
                        cursor += 1;
                    }
                    b'[' => {
                        closers.push(b']');
                        cursor += 1;
                    }
                    b'}' | b']' => {
                        if closers.last() != Some(&bytes[cursor]) {
                            bail!("mismatched JSON closing delimiter at byte {cursor}");
                        }
                        closers.pop();
                        cursor += 1;
                        if closers.is_empty() {
                            return Ok(cursor);
                        }
                    }
                    _ => cursor += 1,
                }
            }
            bail!("unterminated JSON value at byte {start}")
        }
        Some(_) => {
            let mut cursor = start;
            while cursor < bytes.len()
                && !matches!(
                    bytes[cursor],
                    b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']'
                )
            {
                cursor += 1;
            }
            Ok(cursor)
        }
        None => bail!("expected JSON value at end of input"),
    }
}

fn parse_json_object_members(content: &str, open: usize) -> Result<Vec<JsonObjectMember>> {
    let bytes = content.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        bail!("expected JSON object at byte {open}");
    }

    let mut members = Vec::new();
    let mut cursor = open + 1;
    loop {
        let prefix_start = cursor;
        let key_start = skip_json_whitespace(bytes, cursor);
        if bytes.get(key_start) == Some(&b'}') {
            return Ok(members);
        }

        let key_end = scan_json_string_end(bytes, key_start)?;
        let key: String = serde_json::from_str(&content[key_start..key_end])?;
        cursor = skip_json_whitespace(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            bail!("expected ':' after JSON object key at byte {cursor}");
        }

        let value_start = skip_json_whitespace(bytes, cursor + 1);
        let value_end = scan_json_value_end(bytes, value_start)?;
        cursor = skip_json_whitespace(bytes, value_end);
        let comma = if bytes.get(cursor) == Some(&b',') {
            let comma = cursor;
            cursor += 1;
            Some(comma)
        } else if bytes.get(cursor) == Some(&b'}') {
            None
        } else {
            bail!("expected ',' or '}}' after JSON object member at byte {cursor}");
        };

        members.push(JsonObjectMember {
            prefix_start,
            key,
            value_start,
            value_end,
            comma,
        });
        if comma.is_none() {
            return Ok(members);
        }
    }
}

/// Splice every `changes` member whose key is in `applied_paths` out of the
/// raw changepack-log text, leaving all other bytes untouched.
///
/// # Errors
/// Returns an error when `content` is not the object-with-`changes`-object
/// shape the rewriter understands, so a hand-edited or future-schema log is
/// rejected rather than silently corrupted.
pub(crate) fn remove_applied_change_spans(
    content: &str,
    applied_paths: &HashSet<PathBuf>,
) -> Result<String> {
    let root_open = skip_json_whitespace(content.as_bytes(), 0);
    let root_members = parse_json_object_members(content, root_open)?;
    let changes = root_members
        .iter()
        .rev()
        .find(|member| member.key == "changes")
        .context("parsed update log is missing its changes object")?;
    let members = parse_json_object_members(content, changes.value_start)?;
    let selected: Vec<bool> = members
        .iter()
        .map(|member| applied_paths.contains(Path::new(&member.key)))
        .collect();

    let mut removals = Vec::new();
    let mut cursor = 0;
    while cursor < members.len() {
        if !selected[cursor] {
            cursor += 1;
            continue;
        }

        let run_start = cursor;
        while cursor < members.len() && selected[cursor] {
            cursor += 1;
        }
        let run_end = cursor;
        if run_end < members.len() {
            let comma = members[run_end - 1]
                .comma
                .context("selected non-final JSON member is missing its comma")?;
            removals.push((members[run_start].prefix_start, comma + 1));
        } else if run_start == 0 {
            removals.push((members[0].prefix_start, members[run_end - 1].value_end));
        } else {
            let previous_comma = members[run_start - 1]
                .comma
                .context("JSON member before selected final run is missing its comma")?;
            removals.push((previous_comma, members[run_end - 1].value_end));
        }
    }

    let removed_len: usize = removals.iter().map(|(start, end)| end - start).sum();
    let mut output = String::with_capacity(content.len() - removed_len);
    let mut copied_through = 0;
    for (start, end) in removals {
        output.push_str(&content[copied_through..start]);
        copied_through = end;
    }
    output.push_str(&content[copied_through..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the byte-preserving rewriter directly on `content` and return the
    /// flattened error chain.
    ///
    /// `clear_applied_update_logs` runs `serde_json::from_str` as a classifier
    /// before it ever reaches the rewriter, so these malformed inputs can only
    /// be driven through the module-private entry point. The guards are what
    /// keeps a hand-edited or future-schema changepack log from being silently
    /// rewritten into corrupted bytes, so each one is pinned to its message.
    fn scanner_error(content: &str) -> String {
        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let error = remove_applied_change_spans(content, &applied_paths)
            .expect_err("malformed JSON must be rejected instead of rewritten");
        format!("{error:#}")
    }

    #[test]
    fn remove_applied_change_spans_rejects_non_object_root() {
        assert_eq!(
            scanner_error(r#"["packages/a/package.json"]"#),
            "expected JSON object at byte 0"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_unterminated_root_object() {
        // The root brace opens and input ends, so the member loop asks for a
        // key that is not there.
        assert_eq!(scanner_error("{"), "expected JSON string at byte 1");
    }

    #[test]
    fn remove_applied_change_spans_rejects_unquoted_member_key() {
        assert_eq!(
            scanner_error("{changes: {}}"),
            "expected JSON string at byte 1"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_unterminated_string_key() {
        assert_eq!(
            scanner_error(r#"{"changes"#),
            "unterminated JSON string at byte 1"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_invalid_escape_in_key() {
        // `scan_json_string_end` skips the escape pair, then `serde_json`
        // rejects it while decoding the key.
        let error = scanner_error(r#"{"\q":1}"#);
        assert!(
            error.contains("invalid escape"),
            "unexpected error text: {error}"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_missing_colon_after_key() {
        assert_eq!(
            scanner_error(r#"{"changes" 1}"#),
            "expected ':' after JSON object key at byte 11"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_missing_member_separator() {
        assert_eq!(
            scanner_error(r#"{"changes":{} "note":1}"#),
            "expected ',' or '}' after JSON object member at byte 14"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_value_at_end_of_input() {
        assert_eq!(
            scanner_error(r#"{"changes":"#),
            "expected JSON value at end of input"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_unterminated_nested_value() {
        assert_eq!(
            scanner_error(r#"{"changes":[1,2"#),
            "unterminated JSON value at byte 11"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_mismatched_closing_delimiter() {
        assert_eq!(
            scanner_error(r#"{"changes":[1,2}"#),
            "mismatched JSON closing delimiter at byte 15"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_root_without_changes_member() {
        assert_eq!(
            scanner_error(r#"{"note":"hand written","date":"2026-01-01"}"#),
            "parsed update log is missing its changes object"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_non_object_changes_value() {
        // `clear_applied_update_logs` filters this shape out up front, but the
        // rewriter must still refuse it rather than mangle the array.
        assert_eq!(
            scanner_error(r#"{"changes":[],"note":"array schema"}"#),
            "expected JSON object at byte 11"
        );
    }
}
