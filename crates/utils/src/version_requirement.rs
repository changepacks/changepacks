use crate::{replace_version_keep_prefix, split_version};

/// Whether a Cargo version requirement must be rewritten so it keeps naming
/// `new_version`.
///
/// This is the "in scope?" decision shared by every local path-dependency
/// rewrite in `changepacks-rust`: a member crate bumped from `0.2.1` to
/// `0.3.0` invalidates `version = "=0.2.1"` in the workspace root's
/// `[workspace.dependencies]`, and Cargo then refuses to resolve the workspace
/// at all. Answering it here — rather than at each call site — keeps ONE
/// definition of which specifiers track a bump and which are left alone.
///
/// Returns `true` — rewrite — when the requirement names a concrete version
/// that the bump moved away from. Returns `false` — leave byte-identical — in
/// three cases:
///
/// 1. The requirement already resolves to `new_version` after the rebuild, so
///    a write would be a no-op that only dirties the document.
/// 2. The requirement carries no version to track (`*`, `latest`, `""`), or is
///    a compound requirement (`">= 0.2, < 0.4"`) whose extra clauses
///    [`replace_version_keep_prefix`] would silently delete. Manifests here are
///    hand-maintained, so destroying a clause is strictly worse than leaving a
///    requirement that Cargo still accepts.
/// 3. The requirement is *less precise* than the new version and still covers
///    it — `"0.2"` going from `0.2.1` to `0.2.2` — so rewriting it to `"0.2.2"`
///    would silently narrow what the author chose to accept.
///
/// Case 3 is a component-prefix test, not a full semver solver: a requirement
/// whose dotted components are all equal to the leading components of a
/// strictly longer `new_version` still admits it under Cargo's default caret
/// semantics (`"0.2"` means `>=0.2.0, <0.3.0`), and any other shape either
/// names a different component (`"0.2.1"` vs `0.2.2`, `"1.0.0"` vs `1.1.0`) or
/// is more precise than the target. Pre-release / build metadata on either
/// side opts out of the shortcut, because Cargo does not admit a pre-release
/// into a requirement that does not itself name one.
#[must_use]
pub fn requirement_needs_rewrite(spec: &str, new_version: &str) -> bool {
    if replace_version_keep_prefix(spec, new_version) == spec {
        return false;
    }
    let (_, requirement) = split_version(spec);
    if !requirement.starts_with(|character: char| character.is_ascii_digit()) {
        return false;
    }
    if requirement
        .bytes()
        .any(|byte| byte == b',' || byte.is_ascii_whitespace())
    {
        return false;
    }
    !requirement_covers(requirement, new_version)
}

/// Whether `requirement` is a strictly shorter component prefix of
/// `new_version`, i.e. still admits it without being rewritten.
fn requirement_covers(requirement: &str, new_version: &str) -> bool {
    if has_semver_suffix(requirement) || has_semver_suffix(new_version) {
        return false;
    }
    let mut new_components = new_version.split('.');
    for component in requirement.split('.') {
        // A requirement component with no counterpart means the requirement is
        // MORE precise than the new version, so it cannot be a prefix of it.
        let Some(new_component) = new_components.next() else {
            return false;
        };
        if component != new_component {
            return false;
        }
    }
    // Every component matched; only a leftover component on the new version
    // makes the requirement the strictly shorter — and therefore still
    // covering — one. Equal length is already handled by the no-op check in
    // `requirement_needs_rewrite`.
    new_components.next().is_some()
}

/// Whether a dotted version carries a pre-release (`-`) or build (`+`) suffix.
fn has_semver_suffix(version: &str) -> bool {
    version.bytes().any(|byte| byte == b'-' || byte == b'+')
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // The regression this exists for: an exact pin the bump moved away from.
    #[case("=0.2.1", "0.3.0", true)]
    #[case("0.2.1", "0.3.0", true)]
    #[case("^0.2.1", "0.3.0", true)]
    #[case("~0.2.1", "0.3.0", true)]
    #[case(">=0.2.1", "0.3.0", true)]
    // A full-precision requirement tracks even a patch bump, which is the
    // long-standing behaviour of the `[workspace.dependencies]` sync.
    #[case("1.0.0", "1.1.0", true)]
    #[case("0.2.1", "0.2.2", true)]
    // Already naming the target: writing would only dirty the document.
    #[case("0.3.0", "0.3.0", false)]
    #[case("=0.3.0", "0.3.0", false)]
    // Less precise and still covering: the author opted into a range.
    #[case("0.2", "0.2.2", false)]
    #[case("=0.2", "0.2.2", false)]
    #[case("1", "1.2.0", false)]
    // Less precise but no longer covering.
    #[case("0.2", "0.3.0", true)]
    #[case("1", "2.0.0", true)]
    // Nothing concrete to track.
    #[case("*", "0.3.0", false)]
    #[case("latest", "0.3.0", false)]
    #[case("", "0.3.0", false)]
    // Compound requirements would lose their extra clauses to a rebuild.
    #[case(">= 0.2, < 0.4", "0.3.0", false)]
    #[case(">=0.2,<0.4", "0.3.0", false)]
    // Pre-release / build metadata opts out of the component-prefix shortcut.
    #[case("0.2", "0.2.2-alpha.1", true)]
    #[case("0.2.1-alpha.1", "0.2.1", true)]
    // More precise than the target: not a prefix, so it tracks.
    #[case("1.0.0.0", "1.0.0", true)]
    fn test_requirement_needs_rewrite(
        #[case] spec: &str,
        #[case] new_version: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(
            requirement_needs_rewrite(spec, new_version),
            expected,
            "requirement_needs_rewrite({spec:?}, {new_version:?})"
        );
    }
}
