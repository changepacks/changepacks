/// Split a version string into prefix and version parts.
///
/// Returns a `(prefix, version)` tuple: the `prefix` is `Some(String)` when
/// the input begins with non-digit characters (e.g. `"^"`, `"~"`, `">="`,
/// `"helloworld-"`) and `None` when the input starts with a digit or contains
/// no digits at all (e.g. `"latest"`, `"*"`). The function is total — every
/// input yields a valid pair — so it does NOT return a `Result`. The
/// previous `Result<(Option<String>, String)>` signature carried an unreached
/// `Err` variant (both match arms already returned `Ok(...)`), so callers
/// wrote `?` / `.unwrap()` against a `None` case that could never happen.
pub fn split_version(version: &str) -> (Option<String>, String) {
    // Byte-level scan: the predicate is `is_ascii_digit()` (a byte-level
    // check), and ASCII digits are single-byte / cannot appear inside a
    // multi-byte UTF-8 sequence, so `.bytes().position(...)` returns the
    // same byte offset as the previous `.char_indices().find(...).map(...)`
    // chain — with less indirection (no per-char UTF-8 decode, no
    // `.map(|(pos, _)| pos)` bookkeeping that discards the char it just
    // decoded). The `Some(pos)` from a byte hit is always a valid char
    // boundary because the digit itself is single-byte, so slicing
    // `version[..pos]` / `version[pos..]` below stays panic-free even on
    // multi-byte prefixes like `λ-1.0.0`.
    let first_digit_pos = version.bytes().position(|b| b.is_ascii_digit());

    match first_digit_pos {
        Some(0) | None => (None, version.to_string()),
        Some(pos) => {
            let prefix = version[..pos].to_string();
            let version_part = version[pos..].to_string();
            (Some(prefix), version_part)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rstest::rstest;

    #[rstest]
    #[case("1.0.0", (None, "1.0.0"))]
    #[case("^1.0.0", (Some("^"), "1.0.0"))]
    #[case("~1.0.0", (Some("~"), "1.0.0"))]
    #[case("1.0.0-alpha.1", (None, "1.0.0-alpha.1"))]
    #[case("1.0.0-alpha.1+build1", (None, "1.0.0-alpha.1+build1"))]
    #[case(">=1.0.0+build1", (Some(">="), "1.0.0+build1"))]
    #[case("helloworld-1.0.2", (Some("helloworld-"), "1.0.2"))]
    #[case("latest", (None, "latest"))]
    #[case("*", (None, "*"))]
    fn test_split_version(#[case] input: &str, #[case] expected: (Option<&str>, &str)) {
        let (prefix, version) = split_version(input);
        assert_eq!(prefix.as_deref(), expected.0);
        assert_eq!(version.as_str(), expected.1);
    }
}
