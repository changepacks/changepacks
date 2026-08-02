//! Gradle build-script dependency lexer.
//!
//! Scans `build.gradle` (Groovy DSL) and `build.gradle.kts` (Kotlin DSL) sources
//! for `project(...)` dependency declarations without a full parser. The scanner
//! tracks enough lexical state to skip comments, string literals (including
//! Groovy slashy and dollar-slashy forms) and to know whether the cursor sits
//! inside a `dependencies { }` block, so only real project dependencies are
//! reported.
//!
//! Extracted verbatim from `finder.rs`; the only external type it needs is
//! [`GradleDialect`].

use std::path::Path;

use crate::version_lexer::GradleDialect;

fn is_gradle_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

fn is_gradle_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

/// Maps a Gradle bracket opener to the closer the scanner must see to balance it.
///
/// # Caller invariant
///
/// Every call site gates on `byte @ (b'(' | b'[' | b'{')`, so only those three
/// bytes ever reach this function. The `debug_assert!` states that invariant
/// explicitly, so a future ungated call site fails loudly under `cargo test`
/// instead of silently mis-tracking bracket nesting. The mapping stays total in
/// release builds — the wildcard keeps returning `b'}'`, so release behaviour is
/// byte-for-byte unchanged and no unreachable panic branch is introduced.
fn gradle_closer_for(open: u8) -> u8 {
    debug_assert!(
        matches!(open, b'(' | b'[' | b'{'),
        "gradle_closer_for expects a Gradle bracket opener, got byte {open:#04x}"
    );
    match open {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        // Unreachable while the caller invariant above holds; retained so the
        // mapping stays total for all 256 byte values.
        _ => b'}',
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SignificantBytes {
    before_last: Option<u8>,
    last: Option<u8>,
}

impl SignificantBytes {
    fn push(&mut self, byte: u8) {
        self.before_last = self.last;
        self.last = Some(byte);
    }

    fn is_member_access(self) -> bool {
        self.last == Some(b'.')
            || matches!(
                (self.before_last, self.last),
                (Some(b'.'), Some(b'&' | b'@')) | (Some(b':'), Some(b':'))
            )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum GradlePreviousToken {
    #[default]
    StatementStart,
    ExpressionStart,
    ExpressionEnd,
}

#[derive(Clone, Copy, Debug, Default)]
struct GradleLexState {
    significant: SignificantBytes,
    previous: GradlePreviousToken,
}

impl GradleLexState {
    fn slashy_allowed(self) -> bool {
        self.previous != GradlePreviousToken::ExpressionEnd
    }

    fn is_member_access(self) -> bool {
        self.significant.is_member_access()
    }

    fn allows_recovery_candidate(self) -> bool {
        self.previous == GradlePreviousToken::ExpressionEnd
    }

    fn mark_literal(&mut self) {
        self.significant.push(b'v');
        self.previous = GradlePreviousToken::ExpressionEnd;
    }

    fn mark_identifier(&mut self, identifier: &[u8]) {
        if let Some(&last) = identifier.last() {
            self.significant.push(last);
        }
        self.previous = if groovy_identifier_allows_expression(identifier) {
            GradlePreviousToken::ExpressionStart
        } else {
            GradlePreviousToken::ExpressionEnd
        };
    }

    fn mark_byte(&mut self, byte: u8) {
        self.significant.push(byte);
        self.previous = match byte {
            b';' => GradlePreviousToken::StatementStart,
            b'(' | b'[' | b'{' | b',' | b':' | b'=' | b'?' | b'+' | b'-' | b'*' | b'/' | b'%'
            | b'!' | b'&' | b'|' | b'^' | b'<' | b'>' | b'~' => {
                GradlePreviousToken::ExpressionStart
            }
            _ => GradlePreviousToken::ExpressionEnd,
        };
    }

    fn mark_statement_start(&mut self) {
        self.previous = GradlePreviousToken::StatementStart;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GradleIdentifierContext {
    Dependencies,
    DependencyHandlerMember,
    Other,
}

#[derive(Clone, Copy, Debug)]
struct GradlePendingIdentifier {
    context: GradleIdentifierContext,
    begins_dependency_statement: bool,
}

#[derive(Clone, Copy, Debug)]
enum GradleDependencyDelimiter {
    Parenthesis { dependency_declaration: bool },
    Bracket,
    Block { dependencies: bool },
}

#[derive(Debug)]
struct GradleDependencyContext {
    delimiters: Vec<GradleDependencyDelimiter>,
    pending_identifier: Option<GradlePendingIdentifier>,
    dependency_handler_member_access: bool,
    dependency_statement: bool,
    statement_start: bool,
}

impl Default for GradleDependencyContext {
    fn default() -> Self {
        Self {
            delimiters: Vec::new(),
            pending_identifier: None,
            dependency_handler_member_access: false,
            dependency_statement: false,
            statement_start: true,
        }
    }
}

impl GradleDependencyContext {
    /// True only when the innermost open delimiter is a `dependencies { }` block.
    ///
    /// This is a top-of-stack test: searching for the last matching delimiter and
    /// then requiring `position + 1 == len` is satisfiable only by
    /// `position == len - 1`, i.e. by the final element, so inspecting
    /// [`Vec::last`] alone is equivalent. Keeping it O(1) matters because the
    /// method runs once per identifier token via [`Self::mark_identifier`], plus
    /// once per `(` via [`Self::open_parenthesis`] and once per `project(` via
    /// [`Self::allows_project_dependency`]; the previous form walked the whole
    /// delimiter stack on every miss, which is the common case inside `plugins`,
    /// `android` or any nested bracket.
    fn is_directly_in_dependencies_block(&self) -> bool {
        matches!(
            self.delimiters.last(),
            Some(GradleDependencyDelimiter::Block { dependencies: true })
        )
    }

    fn allows_project_dependency(&self) -> bool {
        self.delimiters.iter().any(|delimiter| {
            matches!(
                delimiter,
                GradleDependencyDelimiter::Parenthesis {
                    dependency_declaration: true
                }
            )
        }) || (self.is_directly_in_dependencies_block() && self.dependency_statement)
    }

    fn mark_identifier(&mut self, identifier: &[u8], member_access: bool) {
        let begins_dependency_statement =
            self.is_directly_in_dependencies_block() && self.statement_start && !member_access;
        if begins_dependency_statement {
            self.dependency_statement = true;
        }

        let context = if self.dependency_handler_member_access {
            GradleIdentifierContext::DependencyHandlerMember
        } else if identifier == b"dependencies" {
            GradleIdentifierContext::Dependencies
        } else {
            GradleIdentifierContext::Other
        };
        self.pending_identifier = Some(GradlePendingIdentifier {
            context,
            begins_dependency_statement,
        });
        self.dependency_handler_member_access = false;
        self.statement_start = false;
    }

    fn mark_literal(&mut self) {
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.statement_start = false;
    }

    fn open_parenthesis(&mut self) {
        let dependency_handler_call = self.pending_identifier.is_some_and(|identifier| {
            identifier.context == GradleIdentifierContext::DependencyHandlerMember
        });
        let dependency_declaration = dependency_handler_call
            || (self.is_directly_in_dependencies_block() && self.dependency_statement);
        self.delimiters
            .push(GradleDependencyDelimiter::Parenthesis {
                dependency_declaration,
            });
        self.mark_expression();
    }

    fn open_bracket(&mut self) {
        self.delimiters.push(GradleDependencyDelimiter::Bracket);
        self.mark_expression();
    }

    fn open_block(&mut self) {
        let dependencies = self
            .pending_identifier
            .is_some_and(|identifier| identifier.context == GradleIdentifierContext::Dependencies);
        self.delimiters
            .push(GradleDependencyDelimiter::Block { dependencies });
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.dependency_statement = false;
        self.statement_start = true;
    }

    fn close_parenthesis(&mut self) {
        if matches!(
            self.delimiters.last(),
            Some(GradleDependencyDelimiter::Parenthesis { .. })
        ) {
            self.delimiters.pop();
        }
        self.mark_expression();
    }

    fn close_bracket(&mut self) {
        if matches!(
            self.delimiters.last(),
            Some(GradleDependencyDelimiter::Bracket)
        ) {
            self.delimiters.pop();
        }
        self.mark_expression();
    }

    fn close_block(&mut self) {
        if matches!(
            self.delimiters.last(),
            Some(GradleDependencyDelimiter::Block { .. })
        ) {
            self.delimiters.pop();
        }
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.dependency_statement = false;
        self.statement_start = false;
    }

    fn mark_byte(&mut self, byte: u8) {
        if byte == b'.' {
            self.dependency_handler_member_access =
                self.pending_identifier.is_some_and(|identifier| {
                    identifier.context == GradleIdentifierContext::Dependencies
                });
            self.pending_identifier = None;
            self.statement_start = false;
            return;
        }

        if byte == b';' {
            self.pending_identifier = None;
            self.dependency_handler_member_access = false;
            self.dependency_statement = false;
            self.statement_start = true;
            return;
        }

        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        if matches!(byte, b'=' | b':') {
            self.dependency_statement = false;
        }
        self.statement_start = false;
    }

    fn mark_line_break(&mut self) {
        let continues_identifier = self.pending_identifier.is_some_and(|identifier| {
            identifier.begins_dependency_statement
                || identifier.context != GradleIdentifierContext::Other
        });
        if !continues_identifier {
            self.pending_identifier = None;
            self.dependency_statement = false;
        }
        self.dependency_handler_member_access = false;
        self.statement_start = true;
    }

    fn mark_expression(&mut self) {
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.statement_start = false;
    }

    fn recover_after_malformed_call(&mut self) {
        if let Some(dependencies) = self.delimiters.iter().rposition(|delimiter| {
            matches!(
                delimiter,
                GradleDependencyDelimiter::Block { dependencies: true }
            )
        }) {
            self.delimiters.truncate(dependencies + 1);
        } else {
            self.delimiters.clear();
        }
        self.pending_identifier = None;
        self.dependency_handler_member_access = false;
        self.dependency_statement = false;
        self.statement_start = true;
    }
}

fn gradle_identifier_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while bytes
        .get(end)
        .copied()
        .is_some_and(is_gradle_identifier_byte)
    {
        end += 1;
    }
    end
}

/// Returns the end offset of the identifier starting at `cursor`, or `None`
/// when `cursor` does not sit on an identifier start byte.
///
/// The three lexer loops that walk raw Gradle bytes (`gradle_quarantine_resume`,
/// `scan_gradle_call` and `extract_gradle_project_dependencies`) all need the
/// same "is this an identifier, and where does it end" step before feeding the
/// span to their lexical state, so the guard lives here once.
fn gradle_identifier_span(bytes: &[u8], cursor: usize) -> Option<usize> {
    bytes
        .get(cursor)
        .copied()
        .is_some_and(is_gradle_identifier_start)
        .then(|| gradle_identifier_end(bytes, cursor))
}

/// Single source of truth for the Groovy keywords after which a `/` opens a
/// slashy string instead of being division.
///
/// Both Gradle scanners answer this question for the same dialect: this lexer
/// (byte-oriented) and `version_lexer::identifier_allows_expression` (`&str`,
/// which delegates here). Keeping one table prevents the two scanners from
/// silently disagreeing about where a string literal starts if the keyword set
/// ever changes.
pub(crate) fn groovy_identifier_allows_expression(identifier: &[u8]) -> bool {
    matches!(
        identifier,
        b"assert" | b"case" | b"in" | b"instanceof" | b"new" | b"return" | b"throw" | b"yield"
    )
}

fn quoted_gradle_literal_end(
    bytes: &[u8],
    start: usize,
    triple: bool,
    dialect: GradleDialect,
) -> Option<usize> {
    let quote = *bytes.get(start)?;
    if !matches!(quote, b'\'' | b'"') {
        return None;
    }

    let mut cursor = start + if triple { 3 } else { 1 };
    while cursor < bytes.len() {
        if triple && bytes[cursor] == quote {
            let run_start = cursor;
            while bytes.get(cursor) == Some(&quote) {
                cursor += 1;
            }
            let quote_count = cursor - run_start;
            if quote_count >= 3 {
                let mut backslash_count = 0usize;
                let mut previous = run_start;
                while previous > start + 2 && bytes[previous - 1] == b'\\' {
                    previous -= 1;
                    backslash_count += 1;
                }
                if dialect == GradleDialect::Kotlin || backslash_count.is_multiple_of(2) {
                    return Some(cursor);
                }
            }
            continue;
        }

        match bytes[cursor] {
            b'\\' if !triple || dialect == GradleDialect::Groovy => {
                cursor = (cursor + 2).min(bytes.len());
            }
            byte if byte == quote => return Some(cursor + 1),
            b'\r' | b'\n' if !triple => return None,
            _ => cursor += 1,
        }
    }
    None
}

fn slashy_gradle_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'/' => return Some(cursor + 1),
            _ => cursor += 1,
        }
    }
    None
}

fn dollar_slashy_gradle_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 2;
    while cursor < bytes.len() {
        if bytes.get(cursor..cursor + 2) == Some(b"/$") {
            return Some(cursor + 2);
        }
        cursor += if bytes[cursor] == b'$' && cursor + 1 < bytes.len() {
            2
        } else {
            1
        };
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GradleLiteralScan {
    NotLiteral,
    Complete(usize),
    Unterminated,
}

pub(crate) fn gradle_dependency_dialect(manifest_path: &Path) -> GradleDialect {
    if manifest_path
        .file_name()
        .is_some_and(|name| name == "build.gradle.kts")
    {
        GradleDialect::Kotlin
    } else {
        GradleDialect::Groovy
    }
}

fn scan_gradle_literal(
    bytes: &[u8],
    start: usize,
    dialect: GradleDialect,
    slashy_allowed: bool,
) -> GradleLiteralScan {
    if dialect == GradleDialect::Groovy && bytes.get(start..start + 2) == Some(b"$/") {
        return dollar_slashy_gradle_literal_end(bytes, start)
            .map_or(GradleLiteralScan::Unterminated, GradleLiteralScan::Complete);
    }

    if let Some(quote @ (b'\'' | b'"')) = bytes.get(start).copied() {
        let triple = bytes.get(start + 1) == Some(&quote) && bytes.get(start + 2) == Some(&quote);
        return quoted_gradle_literal_end(bytes, start, triple, dialect)
            .map_or(GradleLiteralScan::Unterminated, GradleLiteralScan::Complete);
    }

    if dialect == GradleDialect::Groovy && slashy_allowed && bytes.get(start) == Some(&b'/') {
        // Groovy `/` is slashy only where the bounded previous-token state
        // allows an expression to start. A slash after a number, identifier,
        // literal, or closing delimiter remains division.
        return slashy_gradle_literal_end(bytes, start)
            .map_or(GradleLiteralScan::NotLiteral, GradleLiteralScan::Complete);
    }

    GradleLiteralScan::NotLiteral
}

fn skip_gradle_comment(bytes: &[u8], start: usize, dialect: GradleDialect) -> Option<usize> {
    match (bytes.get(start), bytes.get(start + 1)) {
        (Some(b'/'), Some(b'/')) => {
            let mut cursor = start + 2;
            while cursor < bytes.len() && !matches!(bytes[cursor], b'\r' | b'\n') {
                cursor += 1;
            }
            Some(cursor)
        }
        (Some(b'/'), Some(b'*')) => {
            let mut cursor = start + 2;
            let mut depth = 1usize;
            while cursor + 1 < bytes.len() {
                match (bytes[cursor], bytes[cursor + 1]) {
                    (b'/', b'*') if dialect == GradleDialect::Kotlin => {
                        depth += 1;
                        cursor += 2;
                    }
                    (b'*', b'/') => {
                        depth -= 1;
                        cursor += 2;
                        if depth == 0 {
                            return Some(cursor);
                        }
                    }
                    _ => cursor += 1,
                }
            }
            Some(bytes.len())
        }
        _ => None,
    }
}

fn skip_gradle_trivia(
    bytes: &[u8],
    mut cursor: usize,
    end: usize,
    dialect: GradleDialect,
) -> usize {
    loop {
        while cursor < end && bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if cursor >= end {
            return end;
        }
        let Some(next) = skip_gradle_comment(bytes, cursor, dialect) else {
            return cursor;
        };
        cursor = next.min(end);
    }
}

fn looks_like_statement_call(bytes: &[u8], start: usize, dialect: GradleDialect) -> bool {
    let mut cursor = skip_gradle_trivia(bytes, start, bytes.len(), dialect);
    if !bytes
        .get(cursor)
        .copied()
        .is_some_and(is_gradle_identifier_start)
    {
        return false;
    }
    while bytes
        .get(cursor)
        .copied()
        .is_some_and(is_gradle_identifier_byte)
    {
        cursor += 1;
    }
    cursor = skip_gradle_trivia(bytes, cursor, bytes.len(), dialect);
    bytes.get(cursor) == Some(&b'(')
}

fn gradle_line_break_end(bytes: &[u8], cursor: usize) -> Option<usize> {
    match bytes.get(cursor) {
        Some(b'\r') if bytes.get(cursor + 1) == Some(&b'\n') => Some(cursor + 2),
        Some(b'\r' | b'\n') => Some(cursor + 1),
        _ => None,
    }
}

fn verified_blank_line_resume(
    bytes: &[u8],
    cursor: usize,
    dialect: GradleDialect,
) -> Option<usize> {
    let mut next = gradle_line_break_end(bytes, cursor)?;
    while matches!(bytes.get(next), Some(b' ' | b'\t')) {
        next += 1;
    }
    next = gradle_line_break_end(bytes, next)?;
    let resume = skip_gradle_trivia(bytes, next, bytes.len(), dialect);
    looks_like_statement_call(bytes, resume, dialect).then_some(resume)
}

#[derive(Debug)]
enum GradleCallScan {
    Complete {
        end: usize,
        arguments: Vec<(usize, usize)>,
    },
    Malformed {
        resume: usize,
    },
}

fn gradle_quarantine_resume(
    bytes: &[u8],
    start: usize,
    dialect: GradleDialect,
    mut expected_closers: Vec<u8>,
) -> usize {
    let mut cursor = start;
    let mut lexical = GradleLexState::default();

    while cursor < bytes.len() {
        if let Some(next) = skip_gradle_comment(bytes, cursor, dialect) {
            cursor = next;
            continue;
        }

        match scan_gradle_literal(bytes, cursor, dialect, lexical.slashy_allowed()) {
            GradleLiteralScan::Complete(next) => {
                cursor = next;
                lexical.mark_literal();
                continue;
            }
            GradleLiteralScan::Unterminated => return bytes.len(),
            GradleLiteralScan::NotLiteral => {}
        }

        if let Some(end) = gradle_identifier_span(bytes, cursor) {
            lexical.mark_identifier(&bytes[cursor..end]);
            cursor = end;
            continue;
        }

        match bytes[cursor] {
            b'\r' | b'\n' => {
                if expected_closers.is_empty()
                    && let Some(resume) = verified_blank_line_resume(bytes, cursor, dialect)
                {
                    return resume;
                }
                cursor = gradle_line_break_end(bytes, cursor).unwrap_or(cursor + 1);
                if !matches!(expected_closers.last(), Some(b')' | b']')) {
                    lexical.mark_statement_start();
                }
            }
            byte @ (b'(' | b'[' | b'{') => {
                expected_closers.push(gradle_closer_for(byte));
                lexical.mark_byte(byte);
                cursor += 1;
            }
            byte @ (b')' | b']' | b'}') => {
                if expected_closers.last() == Some(&byte) {
                    expected_closers.pop();
                }
                lexical.mark_byte(byte);
                cursor += 1;
            }
            byte if byte.is_ascii_whitespace() => cursor += 1,
            byte => {
                lexical.mark_byte(byte);
                cursor += 1;
            }
        }
    }

    bytes.len()
}

fn malformed_gradle_call(
    bytes: &[u8],
    quarantine_start: usize,
    dialect: GradleDialect,
    expected_closers: Vec<u8>,
) -> GradleCallScan {
    GradleCallScan::Malformed {
        resume: gradle_quarantine_resume(bytes, quarantine_start, dialect, expected_closers),
    }
}

fn scan_gradle_call(bytes: &[u8], open: usize, dialect: GradleDialect) -> GradleCallScan {
    let mut cursor = open + 1;
    let mut expected_closers = vec![b')'];
    let mut arguments = Vec::new();
    let mut argument_start = cursor;
    let mut lexical = GradleLexState::default();
    lexical.mark_byte(b'(');
    let mut recovery_candidate = None;

    while cursor < bytes.len() {
        if let Some(next) = skip_gradle_comment(bytes, cursor, dialect) {
            cursor = next;
            continue;
        }

        match scan_gradle_literal(bytes, cursor, dialect, lexical.slashy_allowed()) {
            GradleLiteralScan::Complete(next) => {
                cursor = next;
                lexical.mark_literal();
                continue;
            }
            GradleLiteralScan::Unterminated => {
                if let Some(resume) = recovery_candidate {
                    return GradleCallScan::Malformed { resume };
                }
                return malformed_gradle_call(bytes, cursor, dialect, expected_closers);
            }
            GradleLiteralScan::NotLiteral => {}
        }

        if let Some(end) = gradle_identifier_span(bytes, cursor) {
            lexical.mark_identifier(&bytes[cursor..end]);
            cursor = end;
            continue;
        }

        match bytes[cursor] {
            b'\r' | b'\n' => {
                if expected_closers.len() == 1
                    && lexical.allows_recovery_candidate()
                    && let Some(resume) = verified_blank_line_resume(bytes, cursor, dialect)
                {
                    recovery_candidate.get_or_insert(resume);
                }
                cursor = gradle_line_break_end(bytes, cursor).unwrap_or(cursor + 1);
            }
            byte @ (b'(' | b'[' | b'{') => {
                expected_closers.push(gradle_closer_for(byte));
                lexical.mark_byte(byte);
                cursor += 1;
            }
            byte @ (b')' | b']' | b'}') => {
                if expected_closers.last() != Some(&byte) {
                    if let Some(resume) = recovery_candidate {
                        return GradleCallScan::Malformed { resume };
                    }
                    return malformed_gradle_call(bytes, cursor + 1, dialect, expected_closers);
                }
                if expected_closers.len() == 1 {
                    arguments.push((argument_start, cursor));
                    return GradleCallScan::Complete {
                        end: cursor + 1,
                        arguments,
                    };
                }
                expected_closers.pop();
                lexical.mark_byte(byte);
                cursor += 1;
            }
            b',' if expected_closers.len() == 1 => {
                arguments.push((argument_start, cursor));
                argument_start = cursor + 1;
                lexical.mark_byte(b',');
                cursor += 1;
            }
            byte if byte.is_ascii_whitespace() => cursor += 1,
            byte => {
                lexical.mark_byte(byte);
                cursor += 1;
            }
        }
    }

    if let Some(resume) = recovery_candidate {
        return GradleCallScan::Malformed { resume };
    }
    malformed_gradle_call(bytes, bytes.len(), dialect, expected_closers)
}

fn gradle_assignment(
    content: &str,
    start: usize,
    end: usize,
    dialect: GradleDialect,
) -> Option<(&str, usize)> {
    let bytes = content.as_bytes();
    let mut cursor = skip_gradle_trivia(bytes, start, end, dialect);
    let identifier_start = cursor;
    if !bytes
        .get(cursor)
        .copied()
        .is_some_and(is_gradle_identifier_start)
    {
        return None;
    }
    while cursor < end && is_gradle_identifier_byte(bytes[cursor]) {
        cursor += 1;
    }
    let identifier = content.get(identifier_start..cursor)?;
    cursor = skip_gradle_trivia(bytes, cursor, end, dialect);
    if !matches!(bytes.get(cursor), Some(b'=' | b':')) {
        return None;
    }
    cursor = skip_gradle_trivia(bytes, cursor + 1, end, dialect);
    Some((identifier, cursor))
}

fn plain_gradle_project_path(
    content: &str,
    start: usize,
    end: usize,
    dialect: GradleDialect,
) -> Option<&str> {
    let bytes = content.as_bytes();
    let string_start = skip_gradle_trivia(bytes, start, end, dialect);
    let quote = *bytes.get(string_start)?;
    if !matches!(quote, b'\'' | b'"')
        || (bytes.get(string_start + 1) == Some(&quote)
            && bytes.get(string_start + 2) == Some(&quote))
    {
        return None;
    }

    let string_end = quoted_gradle_literal_end(bytes, string_start, false, dialect)?;
    if string_end > end || skip_gradle_trivia(bytes, string_end, end, dialect) != end {
        return None;
    }

    let project_path = content.get(string_start + 1..string_end - 1)?;
    (!project_path.contains(['\\', '$']) && project_path.starts_with(':')).then_some(project_path)
}

fn gradle_dependency_from_arguments<'a>(
    content: &'a str,
    arguments: &[(usize, usize)],
    dialect: GradleDialect,
) -> Option<&'a str> {
    let bytes = content.as_bytes();
    let mut candidate_count = 0usize;
    let mut project_path = None;

    for (index, &(start, end)) in arguments.iter().enumerate() {
        let argument_start = skip_gradle_trivia(bytes, start, end, dialect);
        if argument_start == end {
            continue;
        }

        if let Some((name, value_start)) = gradle_assignment(content, start, end, dialect) {
            if name != "path" {
                continue;
            }
            candidate_count += 1;
            if candidate_count > 1 {
                return None;
            }
            project_path = plain_gradle_project_path(content, value_start, end, dialect);
            continue;
        }

        let looks_positional =
            index == 0 || matches!(bytes.get(argument_start), Some(b'\'' | b'"' | b'/' | b'$'));
        if looks_positional {
            candidate_count += 1;
            if candidate_count > 1 || index != 0 {
                return None;
            }
            project_path = plain_gradle_project_path(content, argument_start, end, dialect);
        }
    }

    (candidate_count == 1).then_some(project_path).flatten()
}

pub(crate) fn extract_gradle_project_dependencies(
    content: &str,
    dialect: GradleDialect,
) -> Vec<&str> {
    const PROJECT_CALL: &[u8] = b"project";

    let bytes = content.as_bytes();
    let mut dependencies = Vec::new();
    let mut cursor = 0usize;
    let mut lexical = GradleLexState::default();
    let mut dependency_context = GradleDependencyContext::default();
    let mut continuation_group_depth = 0usize;

    while cursor < bytes.len() {
        if let Some(next) = skip_gradle_comment(bytes, cursor, dialect) {
            cursor = next;
            continue;
        }

        match scan_gradle_literal(bytes, cursor, dialect, lexical.slashy_allowed()) {
            GradleLiteralScan::Complete(next) => {
                cursor = next;
                lexical.mark_literal();
                dependency_context.mark_literal();
                continue;
            }
            GradleLiteralScan::Unterminated => break,
            GradleLiteralScan::NotLiteral => {}
        }

        let token_end = cursor + PROJECT_CALL.len();
        // The `while cursor < bytes.len()` loop invariant makes `bytes[cursor..]` infallible.
        let is_project_call = bytes[cursor..].starts_with(PROJECT_CALL)
            && cursor
                .checked_sub(1)
                .and_then(|previous| bytes.get(previous))
                .is_none_or(|byte| !is_gradle_identifier_byte(*byte))
            && bytes
                .get(token_end)
                .is_none_or(|byte| !is_gradle_identifier_byte(*byte));

        if is_project_call {
            let open = skip_gradle_trivia(bytes, token_end, bytes.len(), dialect);
            if bytes.get(open) == Some(&b'(') {
                let qualified = lexical.is_member_access();
                match scan_gradle_call(bytes, open, dialect) {
                    GradleCallScan::Complete { end, arguments } => {
                        if dependency_context.allows_project_dependency()
                            && !qualified
                            && let Some(dependency) =
                                gradle_dependency_from_arguments(content, &arguments, dialect)
                        {
                            dependencies.push(dependency);
                        }
                        cursor = end;
                        lexical.mark_byte(b')');
                        dependency_context.mark_expression();
                    }
                    GradleCallScan::Malformed { resume } => {
                        cursor = resume.max(open + 1);
                        lexical = GradleLexState::default();
                        dependency_context.recover_after_malformed_call();
                        continuation_group_depth = 0;
                    }
                }
                continue;
            }
        }

        if let Some(end) = gradle_identifier_span(bytes, cursor) {
            dependency_context.mark_identifier(&bytes[cursor..end], lexical.is_member_access());
            lexical.mark_identifier(&bytes[cursor..end]);
            cursor = end;
            continue;
        }

        match bytes[cursor] {
            b'(' => {
                continuation_group_depth += 1;
                dependency_context.open_parenthesis();
                lexical.mark_byte(b'(');
            }
            b'[' => {
                continuation_group_depth += 1;
                dependency_context.open_bracket();
                lexical.mark_byte(b'[');
            }
            b'{' => {
                dependency_context.open_block();
                lexical.mark_byte(b'{');
            }
            b')' => {
                continuation_group_depth = continuation_group_depth.saturating_sub(1);
                dependency_context.close_parenthesis();
                lexical.mark_byte(b')');
            }
            b']' => {
                continuation_group_depth = continuation_group_depth.saturating_sub(1);
                dependency_context.close_bracket();
                lexical.mark_byte(b']');
            }
            b'}' => {
                dependency_context.close_block();
                lexical.mark_byte(b'}');
            }
            b'\r' | b'\n' if continuation_group_depth == 0 => {
                dependency_context.mark_line_break();
                lexical.mark_statement_start();
            }
            byte if byte.is_ascii_whitespace() => {}
            byte => {
                dependency_context.mark_byte(byte);
                lexical.mark_byte(byte);
            }
        }
        cursor += 1;
    }

    dependencies
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_gradle_project_dependencies(content: &str) -> Vec<&str> {
        super::extract_gradle_project_dependencies(content, GradleDialect::Groovy)
    }

    #[test]
    fn test_gradle_closer_for_maps_every_legal_opener() {
        assert_eq!(gradle_closer_for(b'('), b')');
        assert_eq!(gradle_closer_for(b'['), b']');
        assert_eq!(gradle_closer_for(b'{'), b'}');
    }

    #[test]
    fn test_extract_gradle_project_dependencies_simple_kotlin_and_groovy() {
        let content = r#"
dependencies {
    implementation(project(":lib"))
    testImplementation(project(':testing:fixtures'))
    api(project(path = ":core"))
    runtimeOnly(project(path = ':tools:cli'))
    implementation(project(path: ':shared'))
    implementation("org.example:external:1.0.0")
}
"#;

        let dependencies = extract_gradle_project_dependencies(content);

        assert_eq!(
            dependencies,
            vec![
                ":lib",
                ":testing:fixtures",
                ":core",
                ":tools:cli",
                ":shared"
            ]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_with_additional_arguments() {
        let content = r#"
dependencies {
    implementation(project(":libraries:공통", configuration = "shadow"))
    api(project(
        path = ":services:인증",
        configuration = "default",
    ))
    runtimeOnly(project(
        path: ':도구:cli',
        configuration: 'runtimeElements'
    ))
}
"#;

        let dependencies = extract_gradle_project_dependencies(content);

        assert_eq!(
            dependencies,
            vec![":libraries:공통", ":services:인증", ":도구:cli"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_ignores_nested_and_unrelated_expressions() {
        let content = r#"
dependencies {
    implementation(project(findProject(":nested-expression"), configuration = "default"))
    api(project(path = resolvePath(":computed-path"), configuration = "default"))
    runtimeOnly(project(path: choosePath(':computed-groovy'), configuration: 'default'))
    implementation(project(":real", configuration = provider(project(":nested-decoy"))))
    implementation(notproject(":identifier-suffix"))
    implementation(projectFactory(":factory"))
    implementation("org.example:external:1.0.0")
}

val rendered = "project(\":string-decoy\", configuration = \"default\")"
// project(":line-comment-decoy", configuration = "default")
/* project(path = ":block-comment-decoy", configuration = "default") */
"#;

        let dependencies = extract_gradle_project_dependencies(content);

        assert_eq!(dependencies, vec![":real"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_requires_dependency_declaration_context() {
        let content = r#"
project(":configured") {
    description = "project configuration, not a dependency"
}
val kotlinAssignment = project(path = ":assigned-kotlin")
def groovyAssignment = project(path: ':assigned-groovy')

dependencies {
    implementation(project(":real"))
    runtimeOnly(platform(project(path = ":nested-real")))
    testImplementation project(path: ':command-real')
}
dependencies.add("compileOnly", project(":direct-add-real"))
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":real", ":nested-real", ":command-real", ":direct-add-real"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_requires_innermost_dependencies_block() {
        let content = r#"
dependencies {
    implementation(project(":direct"))
    someNestedBlock {
        implementation(project(":nested-block-decoy"))
    }
    plugins {
        id("com.example.plugin")
        implementation(project(":deeply:nested-decoy"))
    }
    implementation(project(":after-nested-blocks"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":direct", ":after-nested-blocks"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_skips_gradle_literals_and_dynamic_paths() {
        let content = r#"
val kotlinRaw = """quoted " text project(":triple-double-decoy") """
def groovyTriple = '''quoted ' text project(':triple-single-decoy') '''
def groovySlashy = /text project(":slashy-decoy") text/
def groovyDollarSlashy = $/text project(":dollar-slashy-decoy") text/$

dependencies {
    implementation(project(":plain:유니코드"))
    api(project(":escaped\\path"))
    runtimeOnly(project(":$interpolated"))
    testImplementation(project(":${computed}"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":plain:유니코드"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_skips_comments_after_division() {
        let content = r#"
val first = numerator / denominator // project(":line-comment-decoy")
val second = numerator / denominator /* project(":block-comment-decoy") */
dependencies {
    implementation(project(":real"))
}
"#;

        assert_eq!(extract_gradle_project_dependencies(content), vec![":real"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_uses_dialect_block_comment_nesting() {
        let groovy = r#"
/* Groovy closes this comment at the first terminator.
   /* project(":groovy-comment-decoy") */
dependencies { implementation(project(":real-groovy")) }
"#;
        assert_eq!(
            super::extract_gradle_project_dependencies(groovy, GradleDialect::Groovy),
            vec![":real-groovy"]
        );

        let kotlin = r#"
/* Kotlin keeps the outer comment open.
   /* project(":kotlin-nested-comment-decoy") */
   project(":kotlin-outer-comment-decoy")
*/
dependencies { implementation(project(":real-kotlin")) }
"#;
        assert_eq!(
            super::extract_gradle_project_dependencies(kotlin, GradleDialect::Kotlin),
            vec![":real-kotlin"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_uses_dialect_triple_quote_escapes() {
        let kotlin = r#"
val ordinary = "project(\":ordinary-string-decoy\")"
val raw = """project(":kotlin-raw-decoy") \"""
dependencies { implementation(project(":real-kotlin")) }
"#;
        let raw_start = kotlin.find("\"\"\"").unwrap();
        let raw_end =
            quoted_gradle_literal_end(kotlin.as_bytes(), raw_start, true, GradleDialect::Kotlin)
                .unwrap();
        assert!(kotlin[raw_end..].starts_with("\ndependencies"));
        assert_eq!(
            super::extract_gradle_project_dependencies(kotlin, GradleDialect::Kotlin),
            vec![":real-kotlin"]
        );

        let groovy = r#"
def triple = """before \""" project(":groovy-triple-decoy") after"""
dependencies { implementation(project(":real-groovy")) }
"#;
        assert_eq!(
            super::extract_gradle_project_dependencies(groovy, GradleDialect::Groovy),
            vec![":real-groovy"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_ignores_grouped_non_dependency_calls() {
        let content = r#"
def quotient = (
    numerator
    / project(":grouped-division")
    / denominator
)
dependencies { implementation(project(":real")) }
"#;

        assert_eq!(extract_gradle_project_dependencies(content), vec![":real"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_skips_contextual_groovy_slashy_strings() {
        let content = r#"
return /project(":return-decoy")/
throw /project(":throw-decoy")/
switch (value) {
    case /project(":case-decoy")/: break
}
assert /project(":assert-decoy")/
yield /project(":yield-decoy")/
consume pattern: /project(":command-expression-decoy")/

def identifierQuotient = numerator / denominator
def numericQuotient = 12 / 3
dependencies {
    implementation(project(":real"))
}
"#;

        assert_eq!(extract_gradle_project_dependencies(content), vec![":real"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_rejects_qualified_calls() {
        let content = r#"
dependencies {
    implementation(receiver.project(":dot"))
    api(receiver?. /* trivia */ project(":safe-navigation"))
    runtimeOnly(receiver*. /* trivia */ project(":spread"))
    testImplementation(receiver.&project(":method-pointer"))
    compileOnly(receiver.@project(":direct-field"))
    implementation(project(":free"))
}
"#;

        assert_eq!(extract_gradle_project_dependencies(content), vec![":free"]);
    }

    #[test]
    fn test_extract_gradle_project_dependencies_splits_top_level_arguments() {
        let content = r#"
dependencies {
    implementation(project(configuration = "default", path = ":late-kotlin"))
    api(project(configuration: 'default', path: ':late-groovy'))
    runtimeOnly(project(
        options = mapOf("nested" to listOf(1, 2), "array" to [3, 4]),
        action = { value -> consume(value, mapOf("inner" to listOf(5, 6))) },
        path = ":balanced:stacked",
    ))
    implementation(project(":first", path = ":duplicate"))
    implementation(project(path = ":one", path = ":two"))
    implementation(project(path = resolvePath(":computed")))
    implementation(project(configuration = "no-path"))
    implementation(project(configuration = "default", ":late-positional"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":late-kotlin", ":late-groovy", ":balanced:stacked"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_keeps_balanced_blank_line_continuations() {
        let content = r#"
dependencies {
    implementation(project(":real",

        selectConfiguration(project(":comma-decoy"))
    ))
    implementation(project(path = choosePath(":computed") +

        selectConfiguration(project(":operator-decoy"))
    ))
    implementation(project(":after-balanced-calls"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":real", ":after-balanced-calls"]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_recovers_after_malformed_calls() {
        let content = r#"
dependencies {
    implementation(project(":unclosed"

    implementation(project(":after-unclosed"))
    implementation(project(path = ":mismatched"])

    implementation(project(":after-mismatch"))
    implementation(project(":another-unclosed"
        project(":nested-malformed-decoy")

    implementation(project(":after-nested-malformed"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![
                ":after-unclosed",
                ":after-mismatch",
                ":after-nested-malformed"
            ]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_quarantines_malformed_spans() {
        let content = r#"
dependencies {
    implementation(project(":bad"] + project(":nested-decoy"))

    implementation(project(":after-blank-line"))
    implementation(project(action = run(); ":bad"] + project(":pre-mismatch-boundary-decoy"))

    implementation(project(":after-pre-mismatch-boundary"))
    implementation(project(":unclosed"
    project(":same-indent-nested-decoy")

    implementation(project(":after-second-blank-line"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![
                ":after-blank-line",
                ":after-pre-mismatch-boundary",
                ":after-second-blank-line"
            ]
        );
    }

    #[test]
    fn test_extract_gradle_project_dependencies_keeps_nested_malformed_remainders_quarantined() {
        let content = r#"
dependencies {
    implementation(project(":bad-provider"] + provider({

        project(":nested-provider-decoy")
    }))

    implementation(project(":after-provider"))
    implementation(project(":bad-closure"] + ({

        project(":nested-closure-decoy")
    }))

    implementation(project(":after-closure"))
}
"#;

        assert_eq!(
            extract_gradle_project_dependencies(content),
            vec![":after-provider", ":after-closure"]
        );
    }
}
