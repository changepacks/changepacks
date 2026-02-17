/// Detects JSON indentation (2-space, 4-space, or tab) from file content.
///
/// Scans content line-by-line to find the first non-empty, non-blank line and measures
/// its leading whitespace. Returns 1 for tabs, 0 for no indentation.
#[must_use]
pub fn detect_indent(content: &str) -> usize {
    let mut indent = 0;
    for line in content.lines() {
        if line.trim().is_empty() || line.trim() == line.trim_end() {
            continue;
        }
        indent = line.len() - line.trim_start().len();
        break;
    }
    indent
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
}
