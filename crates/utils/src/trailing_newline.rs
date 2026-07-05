/// Return `"\n"` when `source` ends with a newline, `""` otherwise.
///
/// The language crates rewrite manifest files by dropping trailing whitespace
/// from the serialized body and then re-appending exactly the terminator the
/// original file had (respecting the "preserve formatting" invariant). This
/// helper spells that policy out in one place instead of a ternary duplicated
/// in every `update_version` implementation.
#[must_use]
pub(crate) const fn trailing_newline(source: &str) -> &'static str {
    if let Some(last) = source.as_bytes().last()
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
pub fn finalize_content(body: &str, original: &str) -> String {
    format!("{}{}", body.trim_end(), trailing_newline(original))
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
    // CRLF also yields `\n` because the final byte is LF — matches the
    // existing `raw.ends_with('\n')` behavior.
    #[case("hello\r\n", "\n")]
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
    //   - `original` has CRLF: yields LF (matches `trailing_newline`'s
    //     documented "final byte is LF wins" behavior).
    #[rstest]
    #[case("hello  \n\n", "content\n", "hello\n")]
    #[case("hello\t\t", "content", "hello")]
    #[case("hello", "content\n", "hello\n")]
    #[case("hello\n", "content\r\n", "hello\n")]
    #[case("", "content\n", "\n")]
    fn test_finalize_content(#[case] body: &str, #[case] original: &str, #[case] expected: &str) {
        assert_eq!(finalize_content(body, original), expected);
    }
}
