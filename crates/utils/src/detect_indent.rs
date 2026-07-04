/// Detects JSON indentation (2-space, 4-space, or tab) from file content.
///
/// Scans content line-by-line to find the first non-empty, non-blank line and measures
/// its leading whitespace. Returns 1 for tabs, 0 for no indentation.
#[must_use]
pub fn detect_indent(content: &str) -> usize {
    for line in content.lines() {
        let stripped = line.trim_start();
        if stripped.is_empty() {
            continue;
        }
        let indent = line.len() - stripped.len();
        if indent > 0 {
            return indent;
        }
    }
    0
}

/// Detects JSON indentation as the actual leading-whitespace string of the
/// first non-blank indented line, borrowed from `content`.
///
/// Unlike [`detect_indent`], which returns a byte count and therefore forces
/// callers to pick a whitespace character (usually ASCII space), this returns
/// the exact indent unit — a single `\t` for tab-indented files, `"    "` for
/// four-space, `"  "` for two-space, etc. Returns `""` when no indented line
/// is found. This lets `write_package_json_version` in `changepacks-node`
/// preserve tab and arbitrary-width indentation instead of silently
/// rewriting every tab-indented `package.json` as single-space.
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
    #[case("    print('Hello, world!');", 4)]
    #[case("{\n  \"foo\": \"bar\"}", 2)]
    #[case("{\n    \"foo\": \"bar\"}", 4)]
    #[case("\tconsole.log('test');", 1)]
    #[case("noindent", 0)]
    #[case("  foo\n    bar", 2)]
    #[case("", 0)]
    #[case("           ", 0)]
    #[case("\n    indented\n   less\n", 4)] // First non-empty, non-blank line counts
    #[case("{\n\t\"key\": \"value\"\n}", 1)] // JSON with tab indentation
    #[case("line1\nline2\nline3", 0)] // No indented lines at all
    #[case("{\n   \"key\": \"value\"\n}", 3)] // 3-space indentation
    #[case("\t\tdeep\n\tshallow", 2)] // Double-tab, first match wins
    #[case("{\n\n\n  \"after_blanks\": true\n}", 2)] // Blank lines before first indented
    fn test_detect_indent(#[case] content: &str, #[case] expected: usize) {
        let indent = detect_indent(content);
        assert_eq!(indent, expected);
    }

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
