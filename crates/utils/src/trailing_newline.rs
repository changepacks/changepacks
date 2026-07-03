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

    #[test]
    fn returns_empty_for_empty_string() {
        assert_eq!(trailing_newline(""), "");
    }

    #[test]
    fn returns_empty_when_no_trailing_newline() {
        assert_eq!(trailing_newline("hello"), "");
        assert_eq!(trailing_newline("a\nb"), "");
    }

    #[test]
    fn returns_newline_when_lf_terminated() {
        assert_eq!(trailing_newline("hello\n"), "\n");
        assert_eq!(trailing_newline("\n"), "\n");
    }

    #[test]
    fn returns_newline_for_crlf_because_final_byte_is_lf() {
        // Matches the existing `raw.ends_with('\n')` behavior (`\r\n` ends with `\n`).
        assert_eq!(trailing_newline("hello\r\n"), "\n");
    }
}
