//! Byte-level JSON span scanner used to rewrite changepack logs in place.
//!
//! `serde_json` round-trips would reflow a hand-formatted changepack log, so
//! [`remove_applied_change_spans`] never re-serializes: it locates the byte
//! spans of the applied `changes` members in the ORIGINAL text and splices
//! them out. Everything the caller does not touch — key order, indentation,
//! spacing, trailing newline — survives byte-for-byte.

use std::{collections::HashSet, path::Path};

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
    applied_paths: &HashSet<&Path>,
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
        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
        let error = remove_applied_change_spans(content, &applied_paths)
            .expect_err("malformed JSON must be rejected instead of rewritten");
        format!("{error:#}")
    }

    /// A hand-formatted changepack log: two-space root indent, four-space
    /// members, `changes` before `note` before `date`, trailing newline.
    ///
    /// The success-path tests below all splice this same text so the assertion
    /// is purely about which bytes the removal range covers.
    const HAND_FORMATTED_LOG: &str = r#"{
  "changes": {
    "packages/a/package.json": "Minor",
    "packages/b/package.json": "Patch"
  },
  "note": "hand formatted note",
  "date": "2026-01-01T00:00:00.000Z"
}
"#;

    /// The same hand-formatted shape with a third `changes` member, so a run
    /// can start after index 0 and still end before the last member.
    ///
    /// Two members can only ever produce a leading, trailing, or whole-object
    /// run; the middle-run removal range needs three.
    const THREE_MEMBER_LOG: &str = r#"{
  "changes": {
    "packages/a/package.json": "Minor",
    "packages/b/package.json": "Patch",
    "packages/c/package.json": "Major"
  },
  "note": "hand formatted note",
  "date": "2026-01-01T00:00:00.000Z"
}
"#;

    /// Splice `applied` out of `content`, asserting the rewriter accepts it.
    fn rewrite(content: &str, applied: &[&str]) -> String {
        let applied_paths: HashSet<&Path> = applied.iter().copied().map(Path::new).collect();
        remove_applied_change_spans(content, &applied_paths)
            .expect("a well-formed changepack log must be rewritten, not rejected")
    }

    #[test]
    fn remove_applied_change_spans_removes_member_through_its_trailing_comma() {
        // Removed run is followed by a kept member, so the run must take its
        // own trailing comma with it and leave the next member's indentation.
        assert_eq!(
            rewrite(HAND_FORMATTED_LOG, &["packages/a/package.json"]),
            r#"{
  "changes": {
    "packages/b/package.json": "Patch"
  },
  "note": "hand formatted note",
  "date": "2026-01-01T00:00:00.000Z"
}
"#
        );
    }

    #[test]
    fn remove_applied_change_spans_empties_the_changes_object() {
        // Every member is applied, so the run starts at index 0 and ends at the
        // last value: only the whitespace before the closing brace survives.
        assert_eq!(
            rewrite(
                HAND_FORMATTED_LOG,
                &["packages/a/package.json", "packages/b/package.json"]
            ),
            r#"{
  "changes": {
  },
  "note": "hand formatted note",
  "date": "2026-01-01T00:00:00.000Z"
}
"#
        );
    }

    #[test]
    fn remove_applied_change_spans_removes_the_comma_before_a_final_member() {
        // The removed run ends the object, so the comma belonging to the KEPT
        // preceding member has to go too or the object is left dangling.
        assert_eq!(
            rewrite(HAND_FORMATTED_LOG, &["packages/b/package.json"]),
            r#"{
  "changes": {
    "packages/a/package.json": "Minor"
  },
  "note": "hand formatted note",
  "date": "2026-01-01T00:00:00.000Z"
}
"#
        );
    }

    #[test]
    fn remove_applied_change_spans_removes_a_middle_run_from_its_own_prefix() {
        // Only the middle member is applied, so the run neither starts at the
        // object opening nor ends the object. The removal must begin at that
        // member's own prefix - the bytes right after the PREVIOUS member's
        // comma - so the kept member before it keeps its value, its comma and
        // the indentation of the kept member after it.
        assert_eq!(
            rewrite(THREE_MEMBER_LOG, &["packages/b/package.json"]),
            r#"{
  "changes": {
    "packages/a/package.json": "Minor",
    "packages/c/package.json": "Major"
  },
  "note": "hand formatted note",
  "date": "2026-01-01T00:00:00.000Z"
}
"#
        );
    }

    #[test]
    fn remove_applied_change_spans_returns_the_input_when_nothing_is_applied() {
        // No member matches, so no removal range is ever pushed and the
        // rewriter must hand back the original bytes - indentation, key order
        // and trailing newline included - rather than a reflowed copy.
        assert_eq!(
            rewrite(THREE_MEMBER_LOG, &["packages/unrelated/package.json"]),
            THREE_MEMBER_LOG
        );
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
