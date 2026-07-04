/// Return `"\n"` when `source` ends with a newline, `""` otherwise.
///
/// The language crates rewrite manifest files by dropping trailing whitespace
/// from the serialized body and then re-appending exactly the terminator the
/// original file had (respecting the "preserve formatting" invariant). This
/// helper spells that policy out in one place instead of a ternary duplicated
/// in every `update_version` implementation.
#[must_use]
pub const fn trailing_newline(source: &str) -> &'static str {
    if let Some(last) = source.as_bytes().last()
        && *last == b'\n'
    {
        "\n"
    } else {
        ""
    }
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
}
