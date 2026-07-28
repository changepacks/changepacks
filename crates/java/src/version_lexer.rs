use crate::version_updater::GradleVersionScope;
use regex::Regex;
use std::ops::Range;
use std::sync::LazyLock;

static KTS_SIMPLE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*version\s*=\s*"([^"\r\n]+)""#).expect("hardcoded regex must compile")
});

static KTS_FALLBACK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*version\s*=\s*project\.findProperty\([^)\r\n]+\)\s*\?:\s*"([^"\r\n]+)""#)
        .expect("hardcoded regex must compile")
});

static GROOVY_ASSIGN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*version\s*=\s*(['"])([^'"\r\n]+)(['"])"#)
        .expect("hardcoded regex must compile")
});

static GROOVY_SPACE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*version\s+(['"])([^'"\r\n]+)(['"])"#).expect("hardcoded regex must compile")
});

static SCRIPT_VERSION_DECLARATION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:(?:project\.)?version(?:\s*=|\s+|\.set\s*\(|\s*\()|(?:(?:project|this)\.)?setVersion\s*\()",
    )
    .expect("hardcoded regex must compile")
});

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BraceScope {
    AllProjects,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GradleDialect {
    Kotlin,
    Groovy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodeKind {
    Script,
    Interpolation { brace_depth: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreviousToken {
    StatementStart,
    ExpressionStart,
    ExpressionEnd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StringKind {
    Quoted {
        quote: u8,
        triple: bool,
        interpolation_dollars: usize,
    },
    Slashy,
    DollarSlashy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LexContext {
    Code {
        kind: CodeKind,
        previous: PreviousToken,
        grouping_depth: usize,
    },
    LineComment,
    BlockComment(usize),
    String(StringKind),
}

pub(crate) struct ScriptCandidates {
    pub(crate) editable: Vec<Range<usize>>,
    pub(crate) has_unsupported: bool,
}

fn scope_is_supported(scopes: &[BraceScope], policy: GradleVersionScope) -> bool {
    scopes.is_empty()
        || (policy == GradleVersionScope::ScriptAndAllProjects
            && scopes == [BraceScope::AllProjects])
}

fn kts_value_range(line: &str) -> Option<Range<usize>> {
    [&*KTS_SIMPLE_PATTERN, &*KTS_FALLBACK_PATTERN]
        .into_iter()
        .find_map(|pattern| {
            pattern
                .captures(line)
                .and_then(|captures| captures.get(1))
                .map(|value| value.start()..value.end())
        })
}

fn groovy_value_range(line: &str) -> Option<Range<usize>> {
    [&*GROOVY_ASSIGN_PATTERN, &*GROOVY_SPACE_PATTERN]
        .into_iter()
        .find_map(|pattern| {
            let captures = pattern.captures(line)?;
            if captures.get(1)?.as_str() != captures.get(3)?.as_str() {
                return None;
            }
            let value = captures.get(2)?;
            Some(value.start()..value.end())
        })
}

fn in_script_code(contexts: &[LexContext]) -> bool {
    matches!(
        contexts,
        [LexContext::Code {
            kind: CodeKind::Script,
            ..
        }]
    )
}

fn set_previous_token(contexts: &mut [LexContext], previous: PreviousToken) {
    if let Some(LexContext::Code {
        previous: current, ..
    }) = contexts.last_mut()
    {
        *current = previous;
    }
}

fn previous_token(contexts: &[LexContext]) -> PreviousToken {
    match contexts.last() {
        Some(LexContext::Code { previous, .. }) => *previous,
        _ => PreviousToken::ExpressionEnd,
    }
}

fn reset_statement_on_newline(contexts: &mut [LexContext]) {
    if let Some(LexContext::Code {
        previous,
        grouping_depth,
        ..
    }) = contexts
        .iter_mut()
        .rev()
        .find(|context| matches!(context, LexContext::Code { .. }))
        && *grouping_depth == 0
    {
        *previous = PreviousToken::StatementStart;
    }
}

fn string_interpolates(dialect: GradleDialect, kind: StringKind) -> bool {
    matches!(
        (dialect, kind),
        (
            GradleDialect::Kotlin,
            StringKind::Quoted { quote: b'"', .. }
        ) | (
            GradleDialect::Groovy,
            StringKind::Quoted { quote: b'"', .. }
        ) | (
            GradleDialect::Groovy,
            StringKind::Slashy | StringKind::DollarSlashy
        )
    )
}

fn identifier_allows_expression(identifier: &str) -> bool {
    matches!(
        identifier,
        "assert" | "case" | "in" | "instanceof" | "new" | "return" | "throw" | "yield"
    )
}

/// Scan only the lexical structure needed to classify Gradle version lines.
///
/// Groovy's `/` is context-sensitive. This scanner treats it as a slashy-string
/// opener only after a `StatementStart` or `ExpressionStart` token category.
/// Identifiers, numbers, strings, closing delimiters, and postfix `++`/`--`
/// produce `ExpressionEnd`, so the following slash is division. Newlines reset
/// to `StatementStart` except inside `()`/`[]`, where the prior category is
/// retained; Groovy's escaped LF and CRLF continuations also retain it. This
/// covers common Gradle assignments and regex operators without pretending to
/// parse the full Groovy command-expression grammar.
///
/// Kotlin dollar-prefixed single- and triple-quoted strings store the opener's
/// dollar count and enter `${...}` code only when the dollar run before `{`
/// meets that count. Closing quote runs are consumed together so valid four-
/// and five-quote closing forms leave one or two quotes as string content.
pub(crate) fn candidate_ranges(
    content: &str,
    policy: GradleVersionScope,
    dialect: GradleDialect,
) -> ScriptCandidates {
    let bytes = content.as_bytes();
    let value_range = match dialect {
        GradleDialect::Kotlin => kts_value_range,
        GradleDialect::Groovy => groovy_value_range,
    };
    let mut ranges = Vec::new();
    let mut scopes = Vec::new();
    let mut contexts = vec![LexContext::Code {
        kind: CodeKind::Script,
        previous: PreviousToken::StatementStart,
        grouping_depth: 0,
    }];
    let mut pending_allprojects = false;
    let mut member_access = false;
    let mut index = 0;
    let mut at_line_start = true;
    let mut has_unsupported = false;

    while index < bytes.len() {
        if at_line_start {
            let line_end = bytes[index..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| index + offset);
            if in_script_code(&contexts) && scope_is_supported(&scopes, policy) {
                let line = &content[index..line_end];
                if let Some(range) = value_range(line) {
                    ranges.push(index + range.start..index + range.end);
                } else if SCRIPT_VERSION_DECLARATION_PATTERN.is_match(line) {
                    has_unsupported = true;
                }
            }
            at_line_start = false;
        }

        match *contexts.last().expect("base code context must remain") {
            LexContext::Code { kind, .. } => match bytes[index] {
                b'/' if bytes.get(index + 1) == Some(&b'/') => {
                    contexts.push(LexContext::LineComment);
                    index += 2;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    contexts.push(LexContext::BlockComment(1));
                    index += 2;
                }
                b'$' if dialect == GradleDialect::Kotlin => {
                    let dollar_start = index;
                    while bytes.get(index) == Some(&b'$') {
                        index += 1;
                    }
                    let quoted = bytes.get(index) == Some(&b'"');
                    let triple = quoted
                        && bytes.get(index + 1) == Some(&b'"')
                        && bytes.get(index + 2) == Some(&b'"');
                    if quoted {
                        set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                        contexts.push(LexContext::String(StringKind::Quoted {
                            quote: b'"',
                            triple,
                            interpolation_dollars: index - dollar_start,
                        }));
                        pending_allprojects = false;
                        member_access = false;
                        index += if triple { 3 } else { 1 };
                    } else {
                        set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                        pending_allprojects = false;
                        member_access = false;
                    }
                }
                b'$' if dialect == GradleDialect::Groovy && bytes.get(index + 1) == Some(&b'/') => {
                    set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                    contexts.push(LexContext::String(StringKind::DollarSlashy));
                    pending_allprojects = false;
                    member_access = false;
                    index += 2;
                }
                quote @ (b'\'' | b'"') => {
                    let triple = bytes.get(index + 1) == Some(&quote)
                        && bytes.get(index + 2) == Some(&quote);
                    set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                    contexts.push(LexContext::String(StringKind::Quoted {
                        quote,
                        triple,
                        interpolation_dollars: 1,
                    }));
                    pending_allprojects = false;
                    member_access = false;
                    index += if triple { 3 } else { 1 };
                }
                b'{' => {
                    match kind {
                        CodeKind::Script => {
                            let brace_scope = if scopes.is_empty() && pending_allprojects {
                                BraceScope::AllProjects
                            } else {
                                BraceScope::Other
                            };
                            scopes.push(brace_scope);
                            pending_allprojects = false;
                            member_access = false;
                        }
                        CodeKind::Interpolation { .. } => {
                            if let Some(LexContext::Code {
                                kind: CodeKind::Interpolation { brace_depth },
                                ..
                            }) = contexts.last_mut()
                            {
                                *brace_depth += 1;
                            }
                        }
                    }
                    set_previous_token(&mut contexts, PreviousToken::StatementStart);
                    index += 1;
                }
                b'}' => {
                    match kind {
                        CodeKind::Script => {
                            scopes.pop();
                            pending_allprojects = false;
                            member_access = false;
                            set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                        }
                        CodeKind::Interpolation { brace_depth: 1 } => {
                            contexts.pop();
                        }
                        CodeKind::Interpolation { .. } => {
                            if let Some(LexContext::Code {
                                kind: CodeKind::Interpolation { brace_depth },
                                previous,
                                ..
                            }) = contexts.last_mut()
                            {
                                *brace_depth -= 1;
                                *previous = PreviousToken::ExpressionEnd;
                            }
                        }
                    }
                    index += 1;
                }
                b'.' => {
                    if kind == CodeKind::Script {
                        pending_allprojects = false;
                        member_access = true;
                    }
                    set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                    index += 1;
                }
                byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                    let identifier_start = index;
                    index += 1;
                    while bytes
                        .get(index)
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    {
                        index += 1;
                    }
                    let identifier = &content[identifier_start..index];
                    if kind == CodeKind::Script {
                        pending_allprojects = !member_access && identifier == "allprojects";
                        member_access = false;
                    }
                    set_previous_token(
                        &mut contexts,
                        if identifier_allows_expression(identifier) {
                            PreviousToken::ExpressionStart
                        } else {
                            PreviousToken::ExpressionEnd
                        },
                    );
                }
                byte if byte.is_ascii_digit() => {
                    index += 1;
                    while bytes.get(index).is_some_and(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.')
                    }) {
                        index += 1;
                    }
                    set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                }
                b'\\'
                    if dialect == GradleDialect::Groovy
                        && bytes.get(index + 1) == Some(&b'\r')
                        && bytes.get(index + 2) == Some(&b'\n') =>
                {
                    at_line_start = false;
                    index += 3;
                }
                b'\\'
                    if dialect == GradleDialect::Groovy && bytes.get(index + 1) == Some(&b'\n') =>
                {
                    at_line_start = false;
                    index += 2;
                }
                b'\n' => {
                    reset_statement_on_newline(&mut contexts);
                    at_line_start = true;
                    index += 1;
                }
                byte if byte.is_ascii_whitespace() => {
                    index += 1;
                }
                b'/' if dialect == GradleDialect::Groovy => {
                    if previous_token(&contexts) != PreviousToken::ExpressionEnd {
                        set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                        contexts.push(LexContext::String(StringKind::Slashy));
                    } else {
                        set_previous_token(&mut contexts, PreviousToken::ExpressionStart);
                    }
                    if kind == CodeKind::Script {
                        pending_allprojects = false;
                        member_access = false;
                    }
                    index += 1;
                }
                b')' | b']' => {
                    if let Some(LexContext::Code { grouping_depth, .. }) = contexts.last_mut() {
                        *grouping_depth = grouping_depth.saturating_sub(1);
                    }
                    set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                    if kind == CodeKind::Script {
                        pending_allprojects = false;
                        member_access = false;
                    }
                    index += 1;
                }
                b'(' | b'[' => {
                    if let Some(LexContext::Code { grouping_depth, .. }) = contexts.last_mut() {
                        *grouping_depth += 1;
                    }
                    set_previous_token(&mut contexts, PreviousToken::ExpressionStart);
                    if kind == CodeKind::Script {
                        pending_allprojects = false;
                        member_access = false;
                    }
                    index += 1;
                }
                operator @ (b'+' | b'-') if bytes.get(index + 1) == Some(&operator) => {
                    let is_postfix = previous_token(&contexts) == PreviousToken::ExpressionEnd;
                    set_previous_token(
                        &mut contexts,
                        if is_postfix {
                            PreviousToken::ExpressionEnd
                        } else {
                            PreviousToken::ExpressionStart
                        },
                    );
                    if kind == CodeKind::Script {
                        pending_allprojects = false;
                        member_access = false;
                    }
                    index += 2;
                }
                b';' => {
                    set_previous_token(&mut contexts, PreviousToken::StatementStart);
                    if kind == CodeKind::Script {
                        pending_allprojects = false;
                        member_access = false;
                    }
                    index += 1;
                }
                b',' | b':' | b'=' | b'?' | b'+' | b'-' | b'*' | b'%' | b'!' | b'&' | b'|'
                | b'<' | b'>' | b'~' => {
                    set_previous_token(&mut contexts, PreviousToken::ExpressionStart);
                    if kind == CodeKind::Script {
                        pending_allprojects = false;
                        member_access = false;
                    }
                    index += 1;
                }
                _ => {
                    set_previous_token(&mut contexts, PreviousToken::ExpressionEnd);
                    if kind == CodeKind::Script {
                        pending_allprojects = false;
                        member_access = false;
                    }
                    index += 1;
                }
            },
            LexContext::LineComment => {
                if bytes[index] == b'\n' {
                    contexts.pop();
                    reset_statement_on_newline(&mut contexts);
                    at_line_start = true;
                }
                index += 1;
            }
            LexContext::BlockComment(depth) => {
                if dialect == GradleDialect::Kotlin
                    && bytes[index] == b'/'
                    && bytes.get(index + 1) == Some(&b'*')
                {
                    if let Some(LexContext::BlockComment(depth)) = contexts.last_mut() {
                        *depth += 1;
                    }
                    index += 2;
                } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    if depth == 1 {
                        contexts.pop();
                    } else if let Some(LexContext::BlockComment(depth)) = contexts.last_mut() {
                        *depth -= 1;
                    }
                    index += 2;
                } else {
                    if bytes[index] == b'\n' {
                        reset_statement_on_newline(&mut contexts);
                        at_line_start = true;
                    }
                    index += 1;
                }
            }
            LexContext::String(kind) => match kind {
                StringKind::Quoted {
                    quote,
                    triple,
                    interpolation_dollars,
                } => {
                    let interpolates = string_interpolates(dialect, kind);
                    let supports_escape = !triple || dialect == GradleDialect::Groovy;
                    if interpolates && bytes[index] == b'$' {
                        let dollar_start = index;
                        while bytes.get(index) == Some(&b'$') {
                            index += 1;
                        }
                        if index - dollar_start >= interpolation_dollars
                            && bytes.get(index) == Some(&b'{')
                        {
                            contexts.push(LexContext::Code {
                                kind: CodeKind::Interpolation { brace_depth: 1 },
                                previous: PreviousToken::StatementStart,
                                grouping_depth: 0,
                            });
                            index += 1;
                        }
                    } else if supports_escape && bytes[index] == b'\\' {
                        if bytes.get(index + 1) == Some(&b'\n') {
                            at_line_start = true;
                        }
                        index += usize::from(index + 1 < bytes.len()) + 1;
                    } else if triple && bytes[index] == quote {
                        let quote_start = index;
                        while bytes.get(index) == Some(&quote) {
                            index += 1;
                        }
                        if index - quote_start >= 3 {
                            contexts.pop();
                        }
                    } else if !triple && bytes[index] == quote {
                        contexts.pop();
                        index += 1;
                    } else {
                        if bytes[index] == b'\n' {
                            at_line_start = true;
                        }
                        index += 1;
                    }
                }
                StringKind::Slashy => {
                    if bytes[index] == b'$' && bytes.get(index + 1) == Some(&b'{') {
                        contexts.push(LexContext::Code {
                            kind: CodeKind::Interpolation { brace_depth: 1 },
                            previous: PreviousToken::StatementStart,
                            grouping_depth: 0,
                        });
                        index += 2;
                    } else if bytes[index] == b'\\' && bytes.get(index + 1) == Some(&b'/') {
                        index += 2;
                    } else if bytes[index] == b'/' {
                        contexts.pop();
                        index += 1;
                    } else {
                        if bytes[index] == b'\n' {
                            at_line_start = true;
                        }
                        index += 1;
                    }
                }
                StringKind::DollarSlashy => {
                    if bytes[index] == b'$' && bytes.get(index + 1) == Some(&b'{') {
                        contexts.push(LexContext::Code {
                            kind: CodeKind::Interpolation { brace_depth: 1 },
                            previous: PreviousToken::StatementStart,
                            grouping_depth: 0,
                        });
                        index += 2;
                    } else if bytes[index] == b'$'
                        && matches!(bytes.get(index + 1), Some(b'$' | b'/'))
                    {
                        index += 2;
                    } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'$') {
                        contexts.pop();
                        index += 2;
                    } else {
                        if bytes[index] == b'\n' {
                            at_line_start = true;
                        }
                        index += 1;
                    }
                }
            },
        }
    }

    ScriptCandidates {
        editable: ranges,
        has_unsupported,
    }
}

#[cfg(test)]
mod tests {
    use crate::version_updater::{
        GradleVersionScope, update_version_in_groovy, update_version_in_kts, write_gradle_version,
    };

    #[test]
    fn test_update_version_in_kts_simple() {
        let content = r#"
plugins {
    id("java")
}

group = "com.example"
version = "1.0.0"
"#;
        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();
        assert!(updated.contains(r#"version = "1.0.1""#));
    }

    #[test]
    fn test_update_version_in_kts_with_fallback() {
        let content = r#"
group = "com.devfive"
version = project.findProperty("releaseVersion") ?: "1.0.11"
"#;
        let updated =
            update_version_in_kts(content, "1.0.12", GradleVersionScope::ScriptOnly).unwrap();
        assert!(
            updated.contains(r#"version = project.findProperty("releaseVersion") ?: "1.0.12""#)
        );
    }

    #[test]
    fn test_update_version_in_kts_simple_preserves_space_indentation_byte_for_byte() {
        let content = "    version = \"1.0.0\" // keep this comment\r\n";
        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(updated, "    version = \"1.0.1\" // keep this comment\r\n");
    }

    #[test]
    fn test_update_version_in_kts_simple_preserves_tab_indentation_byte_for_byte() {
        let content = "\tversion\t=\t\"1.0.0\"\n";
        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(updated, "\tversion\t=\t\"1.0.1\"\n");
    }

    #[test]
    fn test_update_version_in_kts_fallback_preserves_space_indentation_byte_for_byte() {
        let content = "allprojects {\r\n    version = project.findProperty(\"releaseVersion\") ?: \"1.0.11\" // fallback\r\n}\r\n";
        let updated =
            update_version_in_kts(content, "1.0.12", GradleVersionScope::ScriptAndAllProjects)
                .unwrap();

        assert_eq!(
            updated,
            "allprojects {\r\n    version = project.findProperty(\"releaseVersion\") ?: \"1.0.12\" // fallback\r\n}\r\n"
        );
    }

    #[test]
    fn test_update_version_in_kts_fallback_preserves_tab_indentation_byte_for_byte() {
        let content = "allprojects {\n\tversion\t=\tproject.findProperty(\"releaseVersion\")\t?:\t\"1.0.11\"\n}\n";
        let updated =
            update_version_in_kts(content, "1.0.12", GradleVersionScope::ScriptAndAllProjects)
                .unwrap();

        assert_eq!(
            updated,
            "allprojects {\n\tversion\t=\tproject.findProperty(\"releaseVersion\")\t?:\t\"1.0.12\"\n}\n"
        );
    }

    #[test]
    fn test_update_version_in_groovy_assign() {
        let content = r#"
group = 'com.example'
version = '2.0.0'
"#;
        let updated =
            update_version_in_groovy(content, "2.0.1", GradleVersionScope::ScriptOnly).unwrap();
        assert!(updated.contains("version = '2.0.1'"));
    }

    #[test]
    fn test_update_version_in_groovy_space() {
        let content = r#"
group = 'com.example'
version '3.0.0'
"#;
        let updated =
            update_version_in_groovy(content, "3.0.1", GradleVersionScope::ScriptOnly).unwrap();
        assert!(updated.contains("version '3.0.1'"));
    }

    #[test]
    fn test_update_version_in_groovy_assign_preserves_double_quotes() {
        let content = r#"
group = 'com.example'
version = "2.0.0"
"#;
        let updated =
            update_version_in_groovy(content, "2.0.1", GradleVersionScope::ScriptOnly).unwrap();
        assert!(updated.contains(r#"version = "2.0.1""#));
    }

    #[test]
    fn test_update_version_in_groovy_space_preserves_double_quotes() {
        let content = r#"
group = 'com.example'
version "3.0.0"
"#;
        let updated =
            update_version_in_groovy(content, "3.0.1", GradleVersionScope::ScriptOnly).unwrap();
        assert!(updated.contains(r#"version "3.0.1""#));
    }

    #[test]
    fn test_update_version_in_groovy_assign_preserves_indentation() {
        let content = r#"
    version = '1.0.0'
"#;
        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();
        assert!(updated.contains("    version = '1.0.1'"));
    }

    #[test]
    fn test_update_version_in_kts_ignores_nested_candidates_and_braces_in_comments_and_strings() {
        let content = "plugins {\r\n    version = \"plugin-version\"\r\n}\r\ncustom {\r\n    version = \"custom-version\"\r\n}\r\n// { unmatched comment brace\r\nval template = \"{ unmatched string brace\"\r\n\tversion\t=\t\"1.0.0\" // project\r\n";

        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "plugins {\r\n    version = \"plugin-version\"\r\n}\r\ncustom {\r\n    version = \"custom-version\"\r\n}\r\n// { unmatched comment brace\r\nval template = \"{ unmatched string brace\"\r\n\tversion\t=\t\"1.0.1\" // project\r\n"
        );
    }

    #[test]
    fn test_update_version_in_groovy_ignores_nested_candidates_and_braces_in_comments_and_strings()
    {
        let content = "plugins {\n  version = 'plugin-version'\n}\ncustom {\n  version \"custom-version\"\n}\n/* { unmatched comment brace */\ndef template = '{ unmatched string brace'\n  version  =  '2.0.0' // project\n";

        let updated =
            update_version_in_groovy(content, "2.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "plugins {\n  version = 'plugin-version'\n}\ncustom {\n  version \"custom-version\"\n}\n/* { unmatched comment brace */\ndef template = '{ unmatched string brace'\n  version  =  '2.0.1' // project\n"
        );
    }

    #[test]
    fn test_lexical_groovy_slashy_string_ignores_multiline_decoy_and_escaped_slash() {
        let content = "def pattern = /{ unmatched string brace\nversion = 'slashy-decoy'\nescaped \\/ delimiter\n/\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def pattern = /{ unmatched string brace\nversion = 'slashy-decoy'\nescaped \\/ delimiter\n/\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_lexical_groovy_yield_allows_multiline_slashy_string() {
        let content = "yield /release pattern\nversion = 'slashy-decoy'\n/\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "yield /release pattern\nversion = 'slashy-decoy'\n/\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_lexical_groovy_division_does_not_start_slashy_string() {
        let content = "def quotient = total / divisor\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def quotient = total / divisor\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_lexical_groovy_dollar_slashy_escaped_close_keeps_crlf_decoy_inside_string() {
        let content = "def template = $/\r\nversion = 'before-escape-decoy'\r\n$/$$\r\nversion = 'after-escape-decoy'\r\n{ unmatched string brace\r\n/$\r\nversion = '1.0.0'\r\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def template = $/\r\nversion = 'before-escape-decoy'\r\n$/$$\r\nversion = 'after-escape-decoy'\r\n{ unmatched string brace\r\n/$\r\nversion = '1.0.1'\r\n"
        );
    }

    #[test]
    fn test_lexical_groovy_block_comments_are_not_nested() {
        let content = "/* outer comment\n   /* nested-looking text\n*/\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "/* outer comment\n   /* nested-looking text\n*/\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_lexical_kotlin_block_comments_remain_nested() {
        let content = "/* outer comment\n   /* inner comment\n   version = \"nested-decoy\"\n   */\n*/\nversion = \"1.0.0\"\n";

        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "/* outer comment\n   /* inner comment\n   version = \"nested-decoy\"\n   */\n*/\nversion = \"1.0.1\"\n"
        );
    }

    #[test]
    fn test_lexical_kotlin_interpolation_inner_quotes_do_not_expose_braces() {
        let content = "val template = \"${mapOf(\"close\" to \"}\", \"open\" to \"{\")}\"\nversion = \"1.0.0\"\n";

        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "val template = \"${mapOf(\"close\" to \"}\", \"open\" to \"{\")}\"\nversion = \"1.0.1\"\n"
        );
    }

    #[test]
    fn test_lexical_groovy_interpolation_inner_quotes_do_not_expose_braces() {
        let content = "def template = \"${[close: \"}\", open: \"{\"]}\"\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def template = \"${[close: \"}\", open: \"{\"]}\"\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_lexical_groovy_triple_quote_escape_does_not_close_string() {
        let content = "def template = \"\"\"escaped triple: \\\"\"\"\nversion = 'triple-decoy'\n{ unmatched string brace\n\"\"\"\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def template = \"\"\"escaped triple: \\\"\"\"\nversion = 'triple-decoy'\n{ unmatched string brace\n\"\"\"\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_lexical_kotlin_triple_quote_backslash_does_not_escape_close() {
        let content = "val template = \"\"\"trailing backslash \\\"\"\"\nversion = \"1.0.0\"\n";

        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "val template = \"\"\"trailing backslash \\\"\"\"\nversion = \"1.0.1\"\n"
        );
    }

    #[test]
    fn test_remaining_lexer_kotlin_multidollar_raw_string_keeps_single_dollar_brace_literal() {
        let content = r#"val template = $$"""
${ unmatched literal brace
version = "raw-decoy"
"""
version = "1.0.0"
"#;

        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            r#"val template = $$"""
${ unmatched literal brace
version = "raw-decoy"
"""
version = "1.0.1"
"#
        );
    }

    #[test]
    fn test_remaining_lexer_kotlin_multidollar_raw_string_accepts_three_dollar_interpolation() {
        let content = r#"val template = $$"""prefix $$${mapOf("close" to "}", "open" to "{")} suffix"""
version = "1.0.0"
"#;

        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            r#"val template = $$"""prefix $$${mapOf("close" to "}", "open" to "{")} suffix"""
version = "1.0.1"
"#
        );
    }

    #[test]
    fn test_final_lexer_kotlin_multidollar_single_line_keeps_smaller_dollar_brace_literal() {
        let content = "val template = $$\"${ unmatched literal brace\"\nversion = \"1.0.0\"\n";

        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "val template = $$\"${ unmatched literal brace\"\nversion = \"1.0.1\"\n"
        );
    }

    #[test]
    fn test_final_lexer_kotlin_multidollar_single_line_accepts_matching_arity_interpolation() {
        let content = "val template = $$\"$${mapOf(\"close\" to \"}\", \"open\" to \"{\")}\"\nversion = \"1.0.0\"\n";

        let updated =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "val template = $$\"$${mapOf(\"close\" to \"}\", \"open\" to \"{\")}\"\nversion = \"1.0.1\"\n"
        );
    }

    #[test]
    fn test_final_lexer_kotlin_valid_four_and_five_quote_runs_close_with_final_three() {
        for content in [
            r#"val template = """prefix """"
version = "1.0.0"
"#,
            r#"val template = """prefix """""
version = "1.0.0"
"#,
        ] {
            let updated =
                update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

            assert_eq!(
                updated,
                content.replace("version = \"1.0.0\"", "version = \"1.0.1\"")
            );
        }
    }

    #[test]
    fn test_final_lexer_groovy_valid_four_and_five_quote_runs_close_with_final_three() {
        for content in [
            r#"def template = """prefix """"
version = '1.0.0'
"#,
            r#"def template = """prefix """""
version = '1.0.0'
"#,
        ] {
            let updated =
                update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

            assert_eq!(
                updated,
                content.replace("version = '1.0.0'", "version = '1.0.1'")
            );
        }
    }

    #[test]
    fn test_remaining_lexer_groovy_statement_newline_allows_multiline_slashy_string() {
        let content = "def prior = 1\n/{ unmatched slashy brace\nversion = 'slashy-decoy'\n/\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def prior = 1\n/{ unmatched slashy brace\nversion = 'slashy-decoy'\n/\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_remaining_lexer_groovy_grouped_newlines_keep_numeric_slashes_as_division() {
        let content = "def quotient = (\n    8\n    / 2\n)\ndef values = [\n    8\n    / 2\n]\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def quotient = (\n    8\n    / 2\n)\ndef values = [\n    8\n    / 2\n]\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_remaining_lexer_groovy_postfix_increment_and_decrement_end_expressions() {
        for content in [
            "def quotient = value++ / 2\nversion = '1.0.0'\n",
            "def quotient = value-- / 2\nversion = '1.0.0'\n",
        ] {
            let updated =
                update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

            assert_eq!(
                updated,
                content.replace("version = '1.0.0'", "version = '1.0.1'")
            );
        }
    }

    #[test]
    fn test_final_lexer_groovy_lf_escaped_line_continuation_keeps_division_context() {
        let content = "def quotient = value \\\n    / 2\nversion = '1.0.0'\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def quotient = value \\\n    / 2\nversion = '1.0.1'\n"
        );
    }

    #[test]
    fn test_final_lexer_groovy_crlf_escaped_line_continuation_keeps_division_context() {
        let content = "def quotient = value \\\r\n    / 2\r\nversion = '1.0.0'\r\n";

        let updated =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap();

        assert_eq!(
            updated,
            "def quotient = value \\\r\n    / 2\r\nversion = '1.0.1'\r\n"
        );
    }

    #[test]
    fn test_update_version_in_kts_no_match() {
        let content = r#"
plugins {
    id("java")
}

group = "com.example"
"#;
        let error =
            update_version_in_kts(content, "2.0.0", GradleVersionScope::ScriptOnly).unwrap_err();
        assert!(error.to_string().contains("No supported"));
    }

    #[test]
    fn test_update_version_in_groovy_no_match() {
        let content = r#"
plugins {
    id 'java'
}

group = 'com.example'
"#;
        let error =
            update_version_in_groovy(content, "2.0.0", GradleVersionScope::ScriptOnly).unwrap_err();
        assert!(error.to_string().contains("No supported"));
    }

    #[test]
    fn test_kts_allprojects_requires_project_wide_policy() {
        let content = "allprojects {\n    version = \"1.0.0\"\n}\n";

        let script_only =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptOnly).unwrap_err();
        let project_wide =
            update_version_in_kts(content, "1.0.1", GradleVersionScope::ScriptAndAllProjects)
                .unwrap();

        assert!(script_only.to_string().contains("No supported"));
        assert_eq!(project_wide, "allprojects {\n    version = \"1.0.1\"\n}\n");
    }

    #[test]
    fn test_groovy_rejects_multiple_supported_script_candidates() {
        let content = "version = '1.0.0'\nversion \"other\"\n";

        let error =
            update_version_in_groovy(content, "1.0.1", GradleVersionScope::ScriptAndAllProjects)
                .unwrap_err();

        assert!(error.to_string().contains("Ambiguous"));
        assert!(error.to_string().contains("2"));
    }

    #[tokio::test]
    async fn test_write_gradle_version_errors_when_no_editable_version_exists() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("build.gradle.kts");
        let content = r#"
plugins {
    id("java")
}

group = "com.example"
"#;
        tokio::fs::write(&path, content).await.unwrap();
        let bytes_before = tokio::fs::read(&path).await.unwrap();

        let error = write_gradle_version(&path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        let bytes_after = tokio::fs::read(&path).await.unwrap();
        assert!(
            error
                .to_string()
                .contains("No supported editable version declaration found")
        );
        assert!(error.to_string().contains(&path.display().to_string()));
        assert_eq!(bytes_after, bytes_before);
    }

    #[tokio::test]
    async fn test_write_gradle_version_updates_indented_kts_declaration() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("build.gradle.kts");
        let content = b"\tversion = \"1.0.0\" // preserve\r\n";
        tokio::fs::write(&path, content).await.unwrap();

        write_gradle_version(&path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap();

        let updated = tokio::fs::read(&path).await.unwrap();
        assert_eq!(updated, b"\tversion = \"1.0.1\" // preserve\r\n");
    }

    #[tokio::test]
    async fn test_write_gradle_version_rejects_ambiguous_candidates_without_writing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("build.gradle.kts");
        let content = b"version = \"1.0.0\"\nversion = project.findProperty(\"releaseVersion\") ?: \"1.0.0\"\n";
        tokio::fs::write(&path, content).await.unwrap();

        let error = write_gradle_version(&path, "1.0.1", GradleVersionScope::ScriptAndAllProjects)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("Ambiguous"));
        assert!(error.to_string().contains(&path.display().to_string()));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), content);
    }

    #[tokio::test]
    async fn write_gradle_version_updates_equals_property_preserving_exact_bytes() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"plugins { id(\"java\") }\r\n";
        let properties_content =
            b"# project version\r\n\tversion \t=  1.0.0 \t # release\r\nother=value\r\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap();

        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            b"# project version\r\n\tversion \t=  1.0.1 \t # release\r\nother=value\r\n"
        );
    }

    #[tokio::test]
    async fn write_gradle_version_updates_colon_property_preserving_exact_bytes() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"plugins { id 'java' }\n";
        let properties_content = b"! version: disabled\n  version :\t2.0.0  ! keep\nlast=true\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        write_gradle_version(&build_path, "2.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap();

        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            b"! version: disabled\n  version :\t2.0.1  ! keep\nlast=true\n"
        );
    }

    #[tokio::test]
    async fn write_gradle_version_rejects_missing_properties_without_writing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        let build_content = b"plugins { id(\"java\") }\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();

        let error = write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("No supported editable version"));
        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert!(
            !tokio::fs::try_exists(temp_dir.path().join("gradle.properties"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn write_gradle_version_rejects_duplicate_active_properties_without_writing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"plugins { id(\"java\") }\n";
        let properties_content =
            b"# version=ignored\nversion=1.0.0\n  version : 2.0.0 # duplicate\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        let error = write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("Ambiguous"));
        assert!(error.to_string().contains("2"));
        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            properties_content
        );
    }

    #[tokio::test]
    async fn write_gradle_version_rejects_competing_script_and_properties_without_writing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"version = '1.0.0'\n";
        let properties_content = b"version=1.0.0\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        let error = write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("Ambiguous"));
        assert!(error.to_string().contains("gradle.properties"));
        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            properties_content
        );
    }

    #[tokio::test]
    async fn write_gradle_version_rejects_provider_backed_script_without_writing_properties() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"version = providers.gradleProperty(\"releaseVersion\").get()\n";
        let properties_content = b"version=1.0.0\nreleaseVersion=1.0.0\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        let error = write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("computed or provider-backed"));
        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            properties_content
        );
    }

    #[tokio::test]
    async fn write_gradle_version_rejects_kotlin_project_set_version_without_writing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content =
            b"project.setVersion(providers.gradleProperty(\"releaseVersion\").get())\r\n";
        let properties_content = b"version = 1.0.0 # shadowed\r\nreleaseVersion=2.0.0\r\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        let error = write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("computed or provider-backed"));
        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            properties_content
        );
    }

    #[tokio::test]
    async fn write_gradle_version_rejects_groovy_this_set_version_without_writing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"this.setVersion(providers.gradleProperty('releaseVersion').get())\n";
        let properties_content = b"version: 1.0.0 ! shadowed\nreleaseVersion=2.0.0\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        let error = write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("computed or provider-backed"));
        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            properties_content
        );
    }
}
