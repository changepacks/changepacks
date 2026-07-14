/// Return the complete trailing-whitespace suffix of `source`.
#[must_use]
fn trailing_whitespace(source: &str) -> &str {
    &source[source.trim_end().len()..]
}

/// Assemble the finalized on-disk bytes for a manifest rewrite by dropping
/// only the serializer-generated trailing whitespace from `body`, then
/// re-appending the original source's complete trailing-whitespace suffix.
/// The suffix starts immediately after the original's final non-whitespace
/// character and is therefore preserved byte-for-byte, including repeated or
/// mixed line endings, spaces, tabs, and other Unicode whitespace.
#[must_use]
pub fn finalize_content(mut body: String, original: &str) -> String {
    let trimmed_len = body.trim_end().len();
    body.truncate(trimmed_len);
    body.push_str(trailing_whitespace(original));
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // Empty / no trailing whitespace.
    #[case("", "")]
    #[case("hello", "")]
    #[case("a\nb", "")]
    // Complete whitespace suffixes, including an all-whitespace source.
    #[case("hello\n", "\n")]
    #[case("hello\n\n", "\n\n")]
    #[case("hello\r\n\r\n", "\r\n\r\n")]
    #[case("hello \t\r\n \n", " \t\r\n \n")]
    #[case(" \t\u{000c}\r\n", " \t\u{000c}\r\n")]
    fn test_trailing_whitespace(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(trailing_whitespace(input), expected);
    }

    #[rstest]
    #[case("hello  \n\n", "content\n", "hello\n")]
    #[case("hello  \n", "content\n\n", "hello\n\n")]
    #[case("hello  \n", "content\r\n\r\n", "hello\r\n\r\n")]
    #[case("hello\n", "content \t\r\n \n", "hello \t\r\n \n")]
    #[case("hello\n", "content\u{000c}\t", "hello\u{000c}\t")]
    #[case("hello\t\t", "content", "hello")]
    #[case("hello", " \t\r\n", "hello \t\r\n")]
    fn test_finalize_content(#[case] body: &str, #[case] original: &str, #[case] expected: &str) {
        assert_eq!(finalize_content(body.to_string(), original), expected);
    }

    #[rstest]
    #[case("hello\r\n\r\n", "content\r\n\r\n")]
    #[case("hello\n\n", "content\n\n")]
    #[case("hello \t\r\n \n", "content \t\r\n \n")]
    #[case("hello", "content")]
    fn test_finalize_content_preserves_suffix_on_repeated_calls(
        #[case] expected_body: &str,
        #[case] original: &str,
    ) {
        let once = finalize_content("hello \t\r\n".to_string(), original);
        let twice = finalize_content(once.clone(), &once);

        assert_eq!(once, expected_body);
        assert_eq!(twice, expected_body);
    }
}
