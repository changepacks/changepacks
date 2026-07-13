/// Return the exact newline terminator when `source` ends with CRLF or LF,
/// `""` otherwise.
///
/// The language crates rewrite manifest files by dropping trailing whitespace
/// from the serialized body and then re-appending exactly the terminator the
/// original file had (respecting the "preserve formatting" invariant). This
/// helper spells that policy out in one place instead of a ternary duplicated
/// in every `update_version` implementation.
#[must_use]
pub(crate) const fn trailing_newline(source: &str) -> &'static str {
    let bytes = source.as_bytes();
    if bytes.len() >= 2 && bytes[bytes.len() - 2] == b'\r' && bytes[bytes.len() - 1] == b'\n' {
        "\r\n"
    } else if let Some(last) = bytes.last()
        && *last == b'\n'
    {
        "\n"
    } else {
        ""
    }
}

/// Assemble the finalized on-disk bytes for a manifest rewrite by dropping
/// trailing whitespace from the serialized `body` and re-appending exactly
/// the terminator the `original` source had (via the crate-internal
/// `trailing_newline` helper).
///
/// Consolidates the `format!("{}{}", body.trim_end(), trailing_newline(original))`
/// incantation previously duplicated verbatim in six manifest writers
/// (`write_package_json_version`, `write_pyproject_version`,
/// `write_cargo_package_version`, `RustWorkspace::update_version`,
/// `RustWorkspace::update_workspace_dependencies`, and
/// `write_pubspec_version`) into ONE source of truth for the
/// "preserve trailing-newline shape" policy. Byte-identical output at
/// every call site — every existing "preserves formatting" and
/// "preserves newline" regression test continues to pass unchanged.
#[must_use]
pub fn finalize_content(mut body: String, original: &str) -> String {
    let trimmed_len = body.trim_end().len();
    body.truncate(trimmed_len);
    body.push_str(trailing_newline(original));
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // Empty / no trailing newline: empty string, plain text, mid-string LF.
    #[case("", "")]
    #[case("hello", "")]
    #[case("a\nb", "")]
    // LF terminated: normal and lone-LF.
    #[case("hello\n", "\n")]
    #[case("\n", "\n")]
    // CRLF terminated: normal and lone-CRLF.
    #[case("hello\r\n", "\r\n")]
    #[case("\r\n", "\r\n")]
    fn test_trailing_newline(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(trailing_newline(input), expected);
    }

    // `finalize_content` regressions: locks the exact byte-identical
    // behavior every call site relied on — `body.trim_end()` concatenated
    // with `trailing_newline(original)`. Fixtures cover the four shapes
    // the six call sites can hit:
    //   - `original` has trailing LF, `body` has trailing whitespace: LF
    //     restored (Node/Python/Rust/Dart happy path).
    //   - `original` lacks trailing LF, `body` has trailing whitespace:
    //     no terminator appended (matches pre-existing `""` branch).
    //   - `body` already has no trailing whitespace: still trimmed
    //     idempotently.
    //   - `original` has CRLF: its exact terminator is restored.
    #[rstest]
    #[case("hello  \n\n", "content\n", "hello\n")]
    #[case("hello\t\t", "content", "hello")]
    #[case("hello", "content\n", "hello\n")]
    #[case("hello\n", "content\r\n", "hello\r\n")]
    #[case("hello\r\n\r\n", "content\r\n", "hello\r\n")]
    #[case("", "content\n", "\n")]
    fn test_finalize_content(#[case] body: &str, #[case] original: &str, #[case] expected: &str) {
        assert_eq!(finalize_content(body.to_string(), original), expected);
    }

    #[rstest]
    #[case("hello\r\n\r\n", "content\r\n", "hello\r\n")]
    #[case("hello\n\n", "content\n", "hello\n")]
    #[case("hello  ", "content", "hello")]
    fn test_finalize_content_preserves_terminator_on_repeated_calls(
        #[case] body: &str,
        #[case] original: &str,
        #[case] expected: &str,
    ) {
        let once = finalize_content(body.to_string(), original);
        let twice = finalize_content(once, original);

        assert_eq!(twice, expected);
    }
}
