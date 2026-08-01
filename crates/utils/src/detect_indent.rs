/// Detects JSON indentation as the actual leading-whitespace string of the
/// first non-blank indented line, borrowed from `content`.
///
/// Returns the exact indent unit — a single `\t` for tab-indented files,
/// `"    "` for four-space, `"  "` for two-space, etc. Returns `""` when no
/// indented line is found. This lets `write_package_json_version` in
/// `changepacks-node` preserve tab and arbitrary-width indentation instead of
/// silently rewriting every tab-indented `package.json` as single-space.
///
/// Segments are split on both `\n` and `\r` rather than via [`str::lines`],
/// which terminates on LF only. A classic-Mac, CR-only manifest therefore
/// still reports its real indent instead of `""`. This is byte-identical for
/// LF files, and for CRLF files too: the pair yields one extra empty segment
/// that the existing blank-segment guard already skips.
#[must_use]
pub fn detect_indent_str(content: &str) -> &str {
    for line in content.split(['\n', '\r']) {
        let stripped = line.trim_start();
        if stripped.is_empty() {
            continue;
        }
        let indent_len = line.len() - stripped.len();
        if indent_len > 0 {
            return &line[..indent_len];
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    use rstest::rstest;

    #[rstest]
    #[case("    print('Hello, world!');", "    ")]
    #[case("{\n  \"foo\": \"bar\"}", "  ")]
    #[case("{\n    \"foo\": \"bar\"}", "    ")]
    #[case("\tconsole.log('test');", "\t")]
    #[case("noindent", "")]
    #[case("", "")]
    #[case("           ", "")]
    #[case("{\n\t\"key\": \"value\"\n}", "\t")] // Tab-indented JSON
    #[case("{\n   \"key\": \"value\"\n}", "   ")] // 3-space indent preserved as-is
    #[case("\t\tdeep\n\tshallow", "\t\t")] // Double-tab, first match wins
    #[case("\n    indented\n   less\n", "    ")] // First non-empty line wins
    #[case("{\n\n\n  \"after_blanks\": true\n}", "  ")] // Skip blanks
    #[case("{\r\n  \"key\": \"value\"\r\n}\r\n", "  ")] // CRLF, 2-space
    #[case("{\r\n\t\"key\": \"value\"\r\n}\r\n", "\t")] // CRLF, tab
    #[case("{\r\n\r\n    \"key\": \"value\"\r\n}\r\n", "    ")] // CRLF, skip blank
    #[case("{\r  \"key\": \"value\"\r}\r", "  ")] // bare CR, 2-space
    #[case("{\r\t\"key\": \"value\"\r}\r", "\t")] // bare CR, tab
    #[case("{\r\r    \"key\": \"value\"\r}\r", "    ")] // bare CR, skip blank
    #[case("{\n \t\"key\": \"value\"\n}", " \t")] // Mixed space-then-tab returned verbatim, not normalised
    #[case("{\n\t \"key\": \"value\"\n}", "\t ")] // Mixed tab-then-space returned verbatim, not normalised
    fn test_detect_indent_str(#[case] content: &str, #[case] expected: &str) {
        let indent = detect_indent_str(content);
        assert_eq!(indent, expected);
    }
}
