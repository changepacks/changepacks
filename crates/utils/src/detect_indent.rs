/// Detects JSON indentation as the actual leading-whitespace string of the
/// first non-blank indented line, borrowed from `content`.
///
/// Returns the exact indent unit — a single `\t` for tab-indented files,
/// `"    "` for four-space, `"  "` for two-space, etc. Returns `""` when no
/// indented line is found. This lets `write_package_json_version` in
/// `changepacks-node` preserve tab and arbitrary-width indentation instead of
/// silently rewriting every tab-indented `package.json` as single-space.
#[must_use]
pub fn detect_indent_str(content: &str) -> &str {
    for line in content.lines() {
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
    fn test_detect_indent_str(#[case] content: &str, #[case] expected: &str) {
        let indent = detect_indent_str(content);
        assert_eq!(indent, expected);
    }
}
