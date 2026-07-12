use anyhow::Result;
use changepacks_core::UpdateType;

/// Single anyhow constructor for every "Invalid version format: <v>" error
/// path in `next_version` — the three early-exit branches (the two
/// `split_once('.')` misses and the `patch.contains('.')` guard) plus the
/// `parse` closure's `u64`-parse failure. Routing all four sites through one
/// helper means a future rewording (e.g. adding valid-shape guidance) only
/// touches one location, so the message can never drift between them. Pure
/// code-quality gain — error path only, so `bench_next_version` (which feeds
/// `"10.20.30"` / `"10.20.30+42"`, the happy path) cannot be affected.
fn invalid_version(v: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Invalid version format: {v} (expected MAJOR.MINOR.PATCH, optionally with +build metadata)"
    )
}

/// Compute the next version with the shared "reserve `0.0.0` when
/// unversioned" fallback used by every language crate's
/// `update_version_from_fields` helper (Node, Python, Dart, CSharp).
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
    //     `parse::<u64>()` fails downstream.
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
    // `Some((base, ext))` when the marker is present, `None` otherwise;
    // multi-`+` inputs (`"1.0.0++"`) route the trailing `+` into `ext`,
    // matching the pre-existing round-trip. The extension keeps its own
    // dots and is re-appended verbatim below.
    let (patch, plus_part) = match patch.split_once('+') {
        Some((base, ext)) => (base, Some(ext)),
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

    // Version components are semver-scoped (spec: 32-bit safe), so `usize`
    // was the wrong type for a serialized format: platform-dependent (32 vs
    // 64 bit). `u64` matches semver's practical upper bound and gives us
    // cross-platform determinism for edge inputs. `Display` for `u64` is
    // byte-identical to `Display` for `usize` at the values real semver
    // components hit, so the `format!` outputs stay unchanged.
    let parse = |s: &str| -> Result<u64> { s.parse::<u64>().map_err(|_| invalid_version(version)) };

    // Parse all three numeric base components up front. This validates that
    // major, minor, and patch are all valid u64 integers before any bump is
    // applied. The parse closure routes all failures through `invalid_version`,
    // ensuring consistent error messages across all three components.
    let major_num = parse(major)?;
    let minor_num = parse(minor)?;
    let patch_num = parse(patch)?;

    // Rebuild via `format!` — one allocation for the result string, no
    // per-part heap traffic. Lower components reset to `0` for Major /
    // Minor bumps, matching the previous "reset lower parts to 0" loop.
    //
    // Guard each increment with `checked_add(1)`: a component of exactly
    // `u64::MAX` (18446744073709551615) parses fine and clears every guard
    // above, so a bare `+ 1` would debug-panic on overflow or silently wrap
    // to `0` in release (e.g. a Major bump of `18446744073709551615.0.0`
    // yielding `0.0.0`, a silently wrong version). Route the overflow through
    // the SAME `invalid_version` constructor as the parse/shape errors so the
    // function honors its documented "invalid version format" contract for
    // every structurally-valid input, from one place — no second error path.
    let bump = |n: u64| n.checked_add(1).ok_or_else(|| invalid_version(version));
    let mut result = match update_type {
        UpdateType::Major => format!("{}.0.0", bump(major_num)?),
        UpdateType::Minor => format!("{}.{}.0", major_num, bump(minor_num)?),
        UpdateType::Patch => format!("{}.{}.{}", major_num, minor_num, bump(patch_num)?),
    };

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
    // Multi-`+` inputs: the FIRST `+` is the metadata marker, so any later
    // `+` characters belong to the extension and round-trip verbatim —
    // locks the documented `split_once('+')` placement.
    #[case("1.0.0++", UpdateType::Patch, "1.0.1++")]
    #[case("1.2.3+a+b", UpdateType::Minor, "1.3.0+a+b")]
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
    // Empty input has no `.` to split on, so it errors at the first
    // `split_once('.')` miss — pin that boundary against future parser rewrites.
    #[case("", UpdateType::Patch)]
    fn test_next_version_invalid_input(#[case] version: &str, #[case] update_type: UpdateType) {
        let result = next_version(version, update_type);
        assert!(result.is_err());
    }

    // A component of exactly `u64::MAX` (18446744073709551615) parses cleanly
    // and clears every shape / `.` / `+` guard, so the ONLY thing between it
    // and a debug-build overflow panic (or a release-build silent wrap to `0`,
    // e.g. a Major bump of `18446744073709551615.0.0` yielding `0.0.0`) is the
    // `checked_add(1)` guard on the bumped component. Each update type bumps a
    // DIFFERENT component, so the boundary must be rejected for whichever
    // component that update actually increments — the other two stay small and
    // valid to prove the error comes from the overflow guard, not the parse.
    #[rstest]
    #[case("18446744073709551615.0.0", UpdateType::Major)]
    #[case("1.18446744073709551615.0", UpdateType::Minor)]
    #[case("1.0.18446744073709551615", UpdateType::Patch)]
    fn test_next_version_component_overflow_is_err(
        #[case] version: &str,
        #[case] update_type: UpdateType,
    ) {
        assert!(next_version(version, update_type).is_err());
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
