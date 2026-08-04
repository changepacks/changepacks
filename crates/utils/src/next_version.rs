use anyhow::Result;
use changepacks_core::UpdateType;

/// Single anyhow constructor for every "Invalid version format: <v>" error
/// path in `next_version`.
fn invalid_version(v: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Invalid version format: {v} (expected MAJOR.MINOR.PATCH, optionally with +build metadata)"
    )
}

fn push_incremented_decimal(result: &mut String, component: &str) {
    if let Some(index) = component
        .as_bytes()
        .iter()
        .rposition(|digit| *digit != b'9')
    {
        result.push_str(&component[..index]);
        result.push(char::from(component.as_bytes()[index] + 1));
        result.extend(std::iter::repeat_n('0', component.len() - index - 1));
    } else {
        result.push('1');
        result.extend(std::iter::repeat_n('0', component.len()));
    }
}

/// Compute the next version with the shared "reserve `0.0.0` when
/// unversioned" fallback used by every language crate's
/// `update_version_from_fields` helper (Node, Python, Dart, `CSharp`).
///
/// Thin wrapper over [`next_version`] that folds the previously duplicated
/// two-line prelude
///
/// ```ignore
/// let current_version = version.as_deref().unwrap_or("0.0.0");
/// let new_version = next_version(current_version, update_type)?;
/// ```
///
/// into a single call site. Java's `update_version_from_fields` continues
/// to call `next_version` directly through `update_gradle_version_at`
/// because the version prelude is spread across the file lifecycle there.
/// Rust's `Package` / `Workspace` also stay on `next_version` because
/// their `update_version` bodies do more than the plain prelude
/// (workspace-inheritance guard, workspace-package fan-out).
///
/// # Errors
/// Propagates [`next_version`]'s error when the resolved version string
/// (either `current` unwrapped or the `"0.0.0"` fallback) is not valid
/// semver — the fallback string is a fixed valid semver, so the error
/// arm can only trip on a malformed `current`.
pub fn next_version_or_default(current: Option<&str>, update_type: UpdateType) -> Result<String> {
    next_version(current.unwrap_or("0.0.0"), update_type)
}

/// Calculate the next version based on semver and update type
///
/// # Errors
/// Returns error if the version format is invalid.
pub fn next_version(version: &str, update_type: UpdateType) -> Result<String> {
    // Destructure `major.minor.patch` via two `split_once('.')` calls
    // instead of collecting into a throwaway `Vec<&str>`. The previous
    // `version.split('.').collect::<Vec<&str>>()` allocated a fresh
    // `Vec<&str>` on every call — hot on the criterion
    // `bench_next_version` bench and on every `Package::update_version`
    // / `Workspace::update_version` call across the 6 language crates.
    // Byte-identical to the previous `split.collect + len() == 3` guard:
    //   - `"1.2"` → the second `split_once('.')` returns `None` → error,
    //     matching the old `len() != 3` early return.
    //   - `"1.2.3.4"` → both splits succeed but `patch = "3.4"` still
    //     carries a `'.'`, caught by `patch.contains('.')` → error.
    //   - `"1.2.wrong"` → both splits succeed, `patch = "wrong"`, no `.`,
    //     canonical-digit validation fails downstream.
    let (major, rest) = version
        .split_once('.')
        .ok_or_else(|| invalid_version(version))?;
    let (minor, patch) = rest
        .split_once('.')
        .ok_or_else(|| invalid_version(version))?;

    // Split off the optional `+build` metadata BEFORE the dotted-patch
    // guard below. Semver build metadata is itself a dot-separated
    // identifier list (`1.2.3+4.5`, `1.2.3+build.7` are spec-valid), so
    // the `.`-check must run on the numeric base ONLY — running it on the
    // raw patch component would wrongly reject a valid `"1.2.3+4.5"`
    // (patch component `"3+4.5"`) and abort the whole `update`.
    // `Some((base, ext))` when the marker is present, `None` otherwise.
    // Validate the extension in place so malformed or repeated `+` markers,
    // empty identifiers, and characters outside SemVer's ASCII
    // alphanumeric/hyphen alphabet never reach the round-trip below.
    let (patch, plus_part) = match patch.split_once('+') {
        Some((base, ext))
            if ext.split('.').all(|identifier| {
                !identifier.is_empty()
                    && identifier
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }) =>
        {
            (base, Some(ext))
        }
        Some(_) => return Err(invalid_version(version)),
        None => (patch, None),
    };

    // Reject a still-dotted patch base. A genuine 4th version component
    // like `"1.2.3.4"` leaves `patch = "3.4"` (no `+`, so unchanged by the
    // split above) and trips this guard exactly as before; because build
    // metadata was already peeled off, its dots (`+4.5`) no longer reach
    // here.
    if patch.contains('.') {
        return Err(invalid_version(version));
    }

    // SemVer numeric identifiers must be nonempty ASCII decimal digits in
    // their canonical spelling: zero is valid by itself, while every
    // multi-digit component must start with a non-zero digit. Do not rely on
    // integer parsing for the lexical rule because it accepts a leading `+`
    // and imposes an artificial numeric ceiling.
    if [major, minor, patch].into_iter().any(|component| {
        component.is_empty()
            || !component.bytes().all(|byte| byte.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
    }) {
        return Err(invalid_version(version));
    }

    let mut result = String::with_capacity(version.len() + 1);
    match update_type {
        UpdateType::Major => {
            push_incremented_decimal(&mut result, major);
            result.push_str(".0.0");
        }
        UpdateType::Minor => {
            result.push_str(major);
            result.push('.');
            push_incremented_decimal(&mut result, minor);
            result.push_str(".0");
        }
        UpdateType::Patch => {
            result.push_str(major);
            result.push('.');
            result.push_str(minor);
            result.push('.');
            push_incremented_decimal(&mut result, patch);
        }
    }

    if let Some(p) = plus_part {
        result.push('+');
        result.push_str(p);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("1.0.0", UpdateType::Major, "2.0.0")]
    #[case("1.0.0", UpdateType::Minor, "1.1.0")]
    #[case("1.0.0", UpdateType::Patch, "1.0.1")]
    #[case("2.5.3", UpdateType::Major, "3.0.0")]
    #[case("2.5.3", UpdateType::Minor, "2.6.0")]
    #[case("2.5.3", UpdateType::Patch, "2.5.4")]
    #[case("0.1.0", UpdateType::Major, "1.0.0")]
    #[case("10.20.30", UpdateType::Major, "11.0.0")]
    #[case("10.20.30", UpdateType::Minor, "10.21.0")]
    #[case("10.20.30", UpdateType::Patch, "10.20.31")]
    #[case("10.20.30+1", UpdateType::Patch, "10.20.31+1")]
    // Build metadata is a dot-separated identifier list (semver spec): a
    // dotted `+build` suffix must be accepted and round-tripped verbatim,
    // not rejected by the numeric-patch `.`-guard. Regression anchor for
    // performing the `+` split ahead of that guard.
    #[case("1.2.3+4.5", UpdateType::Patch, "1.2.4+4.5")]
    #[case("1.2.3+4.5", UpdateType::Major, "2.0.0+4.5")]
    #[case("1.2.3+build.7", UpdateType::Minor, "1.3.0+build.7")]
    #[case("1.2.3+build-7.alpha9", UpdateType::Patch, "1.2.4+build-7.alpha9")]
    fn test_next_version(
        #[case] version: &str,
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        let result = next_version(version, update_type).unwrap();
        assert_eq!(result, expected);
    }

    #[rstest]
    #[case("invalid", UpdateType::Major)]
    #[case("1.2", UpdateType::Minor)]
    #[case("1.2.3.4", UpdateType::Patch)]
    #[case("1.2.wrong", UpdateType::Patch)]
    #[case("x.2.3", UpdateType::Patch)]
    #[case("1.y.3", UpdateType::Patch)]
    #[case("1.y.3", UpdateType::Major)]
    #[case("1.y.3", UpdateType::Minor)]
    #[case("1.2.wrong", UpdateType::Major)]
    #[case("1.2.wrong", UpdateType::Minor)]
    // The supported SemVer subset requires canonical numeric components.
    #[case("01.2.3", UpdateType::Patch)]
    #[case("1.02.3", UpdateType::Patch)]
    #[case("1.2.03", UpdateType::Patch)]
    #[case("+1.2.3", UpdateType::Patch)]
    #[case("1.+2.3", UpdateType::Patch)]
    // Pre-release versions remain intentionally unsupported.
    #[case("1.2.3-alpha", UpdateType::Patch)]
    // Build metadata is one nonempty, dot-separated list of identifiers.
    #[case("1.2.3+", UpdateType::Patch)]
    #[case("1.2.3++", UpdateType::Patch)]
    #[case("1.2.3+a+b", UpdateType::Patch)]
    #[case("1.2.3+.build", UpdateType::Patch)]
    #[case("1.2.3+build.", UpdateType::Patch)]
    #[case("1.2.3+build..7", UpdateType::Patch)]
    #[case("1.2.3+build_7", UpdateType::Patch)]
    #[case("1.2.3+build/7", UpdateType::Patch)]
    #[case("1.2.3+build 7", UpdateType::Patch)]
    #[case("1.2.3+béta", UpdateType::Patch)]
    // Empty input has no `.` to split on, so it errors at the first
    // `split_once('.')` miss — pin that boundary against future parser rewrites.
    #[case("", UpdateType::Patch)]
    fn test_next_version_invalid_input(#[case] version: &str, #[case] update_type: UpdateType) {
        let err = next_version(version, update_type).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "Invalid version format: {version} (expected MAJOR.MINOR.PATCH, optionally with +build metadata)"
            )
        );
    }

    #[rstest]
    #[case(
        "18446744073709551616.7.8",
        UpdateType::Major,
        "18446744073709551617.0.0"
    )]
    #[case(
        "18446744073709551616.18446744073709551616.8",
        UpdateType::Minor,
        "18446744073709551616.18446744073709551617.0"
    )]
    #[case(
        "18446744073709551616.18446744073709551616.18446744073709551616+build.0007",
        UpdateType::Patch,
        "18446744073709551616.18446744073709551616.18446744073709551617+build.0007"
    )]
    #[case(
        concat!("9999999999", "9999999999", "9999999999", ".2.3"),
        UpdateType::Major,
        concat!("1", "0000000000", "0000000000", "0000000000", ".0.0")
    )]
    #[case(
        concat!("1234567890.9999999999", "9999999999", "9999999999", ".7"),
        UpdateType::Minor,
        concat!("1234567890.1", "0000000000", "0000000000", "0000000000", ".0")
    )]
    #[case(
        concat!("1234567890.7.9999999999", "9999999999", "9999999999", "+meta.9"),
        UpdateType::Patch,
        concat!(
            "1234567890.7.1",
            "0000000000",
            "0000000000",
            "0000000000",
            "+meta.9"
        )
    )]
    fn test_next_version_arbitrary_length_component(
        #[case] version: &str,
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        assert_eq!(next_version(version, update_type).unwrap(), expected);
    }

    // `next_version_or_default` is public API (re-exported from `lib.rs`)
    // consumed by `bump_version_with`, `display_update` and
    // `gen_changepack_result_map`, but its `None` -> "0.0.0" fallback was
    // only covered transitively through `display_update`. Pin the fallback
    // for every bump kind directly on the function that owns it.
    #[rstest]
    #[case(UpdateType::Major, "1.0.0")]
    #[case(UpdateType::Minor, "0.1.0")]
    #[case(UpdateType::Patch, "0.0.1")]
    fn test_next_version_or_default_none_falls_back_to_zero(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        assert_eq!(
            next_version_or_default(None, update_type).unwrap(),
            expected
        );
    }

    // A present version must be forwarded to `next_version` untouched: the
    // fallback may never shadow a real current version.
    #[rstest]
    #[case("2.5.3", UpdateType::Patch, "2.5.4")]
    #[case("2.5.3", UpdateType::Minor, "2.6.0")]
    #[case("2.5.3", UpdateType::Major, "3.0.0")]
    #[case("1.2.3+build.7", UpdateType::Patch, "1.2.4+build.7")]
    fn test_next_version_or_default_some_delegates_unchanged(
        #[case] current: &str,
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        assert_eq!(
            next_version_or_default(Some(current), update_type).unwrap(),
            expected
        );
        // Same input through the wrapper and through the wrapped function.
        assert_eq!(
            next_version_or_default(Some(current), update_type).unwrap(),
            next_version(current, update_type).unwrap()
        );
    }

    // A malformed `Some` must surface `next_version`'s error, NOT silently
    // degrade to the `"0.0.0"` fallback — that would rewrite a corrupt
    // manifest version to `0.0.1` instead of aborting the update.
    #[rstest]
    #[case("abc", UpdateType::Patch)]
    #[case("1.2", UpdateType::Minor)]
    #[case("1.2.3-alpha", UpdateType::Major)]
    fn test_next_version_or_default_malformed_some_errors(
        #[case] current: &str,
        #[case] update_type: UpdateType,
    ) {
        let err = next_version_or_default(Some(current), update_type).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "Invalid version format: {current} (expected MAJOR.MINOR.PATCH, optionally with +build metadata)"
            )
        );
    }

    // A rejected version (here a pre-release, which `next_version`
    // deliberately does not bump) must explain the ACCEPTED shape, not just
    // report the input as "invalid" — otherwise `1.0.0-alpha.1` reads as a
    // valid semver being wrongly refused. Locks the accepted-format hint.
    #[test]
    fn test_next_version_invalid_message_has_format_hint() {
        let err = next_version("1.0.0-alpha.1", UpdateType::Patch).unwrap_err();
        assert!(
            err.to_string().contains("expected MAJOR.MINOR.PATCH"),
            "error should hint the accepted format, got: {err}"
        );
    }
}
