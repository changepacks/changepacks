use std::fmt;

/// Write `items` into `writer`, inserting `separator` between elements.
///
/// Several diagnostics render a list as `a, b, c`. Each site used to hand-roll
/// the same loop — enumerate the items, emit the separator when the index is
/// non-zero, then append the element — differing only in the sink and in the
/// separator. `fmt::Write` covers both sinks: `fmt::Formatter` implements it,
/// so a `Display` impl streams straight into the formatter with no intermediate
/// `String`, and `String` implements it, so a caller that wants an owned join
/// accumulates into one running buffer instead of a `Vec<String>` plus a
/// `join`. Gating on the element *index* rather than on the sink being
/// non-empty is load-bearing: a leading empty element must still be followed by
/// its separator.
///
/// # Errors
/// Returns the first [`fmt::Error`] reported by `writer`. Writing into a
/// `String` is infallible, so a `String` sink never takes this path; a
/// `fmt::Formatter` sink propagates whatever the underlying writer reports.
pub fn write_separated<W: fmt::Write, T: fmt::Display>(
    writer: &mut W,
    items: impl IntoIterator<Item = T>,
    separator: &str,
) -> fmt::Result {
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            writer.write_str(separator)?;
        }
        write!(writer, "{item}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_separated;

    /// Render `items` into a fresh `String` through the helper under test.
    fn joined<T: std::fmt::Display>(items: impl IntoIterator<Item = T>, separator: &str) -> String {
        let mut buffer = String::new();
        write_separated(&mut buffer, items, separator).expect("String sink is infallible");
        buffer
    }

    #[test]
    fn test_write_separated_empty_writes_nothing() {
        let items: [&str; 0] = [];
        assert_eq!(joined(items.iter(), ", "), "");
    }

    #[test]
    fn test_write_separated_single_omits_the_separator() {
        assert_eq!(joined(["only"].iter(), ", "), "only");
    }

    #[test]
    fn test_write_separated_three_elements_are_separated_pairwise() {
        assert_eq!(joined(["a", "b", "c"].iter(), ", "), "a, b, c");
    }

    /// The separator is gated on the element index, not on the sink being
    /// non-empty: a leading empty element still gets its separator.
    #[test]
    fn test_write_separated_gates_on_index_not_on_sink_emptiness() {
        assert_eq!(joined(["", "b"].iter(), ", "), ", b");
    }

    /// A non-empty prefix already in the sink is preserved, and the first
    /// element is appended without a leading separator.
    #[test]
    fn test_write_separated_appends_to_an_existing_buffer() {
        let mut buffer = String::from("candidates: ");
        write_separated(&mut buffer, ["x", "y"].iter(), ", ").expect("String sink is infallible");
        assert_eq!(buffer, "candidates: x, y");
    }
}
