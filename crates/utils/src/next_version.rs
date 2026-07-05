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
    anyhow::anyhow!("Invalid version format: {v}")
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
    //     `parse::<usize>()` fails downstream.
    let (major, rest) = version
        .split_once('.')
        .ok_or_else(|| invalid_version(version))?;
    let (minor, patch) = rest
        .split_once('.')
        .ok_or_else(|| invalid_version(version))?;
    if patch.contains('.') {
        return Err(invalid_version(version));
    }

    // Optional `+build` suffix on the patch component. Semantics are
    // byte-identical to the previous `split_once('+')` handling:
    // `Some((patch, build))` when the marker is present, `None`
    // otherwise. Multi-`+` inputs (`"1.0.0++"`) route the trailing `+`
    // into `build`, matching the pre-existing round-trip.
    let (patch, plus_part) = match patch.split_once('+') {
        Some((base, ext)) => (base, Some(ext)),
        None => (patch, None),
    };

    // Version components are semver-scoped (spec: 32-bit safe), so `usize`
    // was the wrong type for a serialized format: platform-dependent (32 vs
    // 64 bit). `u64` matches semver's practical upper bound and gives us
    // cross-platform determinism for edge inputs. `Display` for `u64` is
    // byte-identical to `Display` for `usize` at the values real semver
    // components hit, so the `format!` outputs stay unchanged.
    let parse = |s: &str| -> Result<u64> { s.parse::<u64>().map_err(|_| invalid_version(version)) };

    // Rebuild via `format!` — one allocation for the result string, no
    // per-part heap traffic. Lower components reset to `0` for Major /
    // Minor bumps, matching the previous "reset lower parts to 0" loop.
    let mut result = match update_type {
        UpdateType::Major => format!("{}.0.0", parse(major)? + 1),
        UpdateType::Minor => format!("{major}.{}.0", parse(minor)? + 1),
        UpdateType::Patch => format!("{major}.{minor}.{}", parse(patch)? + 1),
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
    fn test_next_version_invalid_input(#[case] version: &str, #[case] update_type: UpdateType) {
        let result = next_version(version, update_type);
        assert!(result.is_err());
    }
}
