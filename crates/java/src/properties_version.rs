use std::ops::Range;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PropertyAssignment {
    Literal(Range<usize>),
    Unsupported,
}

const fn is_property_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | 0x0c)
}

/// Advances `cursor` past every property whitespace byte before `end`.
const fn skip_property_whitespace(content: &[u8], cursor: usize, end: usize) -> usize {
    let mut cursor = cursor;
    while cursor < end && is_property_whitespace(content[cursor]) {
        cursor += 1;
    }
    cursor
}

fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let preceding_backslashes = bytes[..index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    preceding_backslashes % 2 == 1
}

/// Substrings that make a `version` property value computed rather than literal.
///
/// The scan window size is derived from each marker's own length, so editing a
/// marker here can never desynchronise it from a hand-written length.
const NON_LITERAL_MARKERS: [&[u8]; 4] = [b"${", b"providers.", b"project.", b"findProperty("];

fn contains_marker(value: &[u8], marker: &[u8]) -> bool {
    marker.len() <= value.len() && value.windows(marker.len()).any(|window| window == marker)
}

/// A value is literal when it holds no computed marker and does not end in a
/// line continuation.
///
/// `is_escaped(value, value.len())` already answers the continuation question on
/// its own: it counts the trailing backslash run and is only true for an odd
/// run, which implies the run is non-empty and therefore that the final byte is
/// a backslash. Testing `value.last() == Some(b'\\')` alongside it could never
/// change the outcome, and for an empty value the count is `0`, so the dropped
/// `last().is_some_and(..)` wrapper was redundant as well.
fn property_value_is_literal(value: &[u8]) -> bool {
    !NON_LITERAL_MARKERS
        .iter()
        .any(|marker| contains_marker(value, marker))
        && !is_escaped(value, value.len())
}

pub(crate) fn property_assignments(content: &[u8]) -> Vec<PropertyAssignment> {
    let mut assignments = Vec::new();
    let mut line_start = 0;

    while line_start < content.len() {
        let line_end = content[line_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(content.len(), |offset| line_start + offset);
        let logical_end = if line_end > line_start && content[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        let mut cursor = skip_property_whitespace(content, line_start, logical_end);

        if !matches!(content.get(cursor), Some(b'#' | b'!'))
            && content.get(cursor..cursor + b"version".len()) == Some(b"version")
        {
            cursor += b"version".len();
            let separator_start = cursor;
            cursor = skip_property_whitespace(content, cursor, logical_end);
            let had_whitespace_separator = cursor > separator_start;
            let value_start = if matches!(content.get(cursor), Some(b'=' | b':')) {
                cursor += 1;
                cursor = skip_property_whitespace(content, cursor, logical_end);
                Some(cursor)
            } else if had_whitespace_separator && cursor < logical_end {
                Some(cursor)
            } else {
                None
            };
            if let Some(value_start) = value_start {
                let mut value_end = logical_end;
                let mut scan = value_start;
                while scan < logical_end {
                    if matches!(content[scan], b'#' | b'!')
                        && scan > value_start
                        && is_property_whitespace(content[scan - 1])
                        && !is_escaped(content, scan)
                    {
                        value_end = scan;
                        break;
                    }
                    scan += 1;
                }
                while value_end > value_start && is_property_whitespace(content[value_end - 1]) {
                    value_end -= 1;
                }

                if value_start < value_end
                    && property_value_is_literal(&content[value_start..value_end])
                {
                    assignments.push(PropertyAssignment::Literal(value_start..value_end));
                } else {
                    assignments.push(PropertyAssignment::Unsupported);
                }
            }
        }

        line_start = if line_end < content.len() {
            line_end + 1
        } else {
            content.len()
        };
    }

    assignments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version_updater::GradleVersionScope;
    use crate::write_gradle_version;
    use rstest::rstest;

    #[test]
    fn literal_assignment_returns_exact_value_range() {
        let content = b"other=true\r\n\tversion =  1.2.3 \t # release\r\n";

        let assignments = property_assignments(content);

        assert_eq!(assignments, [PropertyAssignment::Literal(24..29)]);
        assert_eq!(&content[24..29], b"1.2.3");
    }

    #[test]
    fn computed_assignment_is_unsupported() {
        let content = b"version=${releaseVersion}\n";

        let assignments = property_assignments(content);

        assert_eq!(assignments, [PropertyAssignment::Unsupported]);
    }

    #[rstest]
    #[case(
        b"version=providers.gradleProperty(\"v\").get()\n",
        &[PropertyAssignment::Unsupported],
        None
    )]
    #[case(b"version=project.version\n", &[PropertyAssignment::Unsupported], None)]
    #[case(b"version=findProperty(\"v\")\n", &[PropertyAssignment::Unsupported], None)]
    #[case(
        b"version=1.2.3-providers\n",
        &[PropertyAssignment::Literal(8..23)],
        Some(b"1.2.3-providers".as_slice())
    )]
    fn computed_property_markers_are_unsupported(
        #[case] content: &[u8],
        #[case] expected: &[PropertyAssignment],
        #[case] expected_literal: Option<&[u8]>,
    ) {
        let assignments = property_assignments(content);

        assert_eq!(assignments, expected);
        let literal = assignments.iter().find_map(|assignment| match assignment {
            PropertyAssignment::Literal(range) => Some(&content[range.start..range.end]),
            PropertyAssignment::Unsupported => None,
        });
        assert_eq!(literal, expected_literal);
    }

    #[test]
    fn commented_assignments_are_ignored() {
        let content = b"# version=1.0.0\n! version:2.0.0\nother=true\n";

        let assignments = property_assignments(content);

        assert!(assignments.is_empty());
    }

    #[rstest]
    #[case(
        b"version 1.2.3\n",
        &[PropertyAssignment::Literal(8..13)]
    )]
    #[case(
        b"version \t 1.2.3 \t # c\n",
        &[PropertyAssignment::Literal(10..15)]
    )]
    #[case(b"version\n", &[])]
    #[case(b"versionSuffix 1.0\n", &[])]
    fn whitespace_separated_assignments_follow_java_properties_rules(
        #[case] content: &[u8],
        #[case] expected: &[PropertyAssignment],
    ) {
        let assignments = property_assignments(content);

        assert_eq!(assignments, expected);
    }

    #[rstest]
    #[case(b"version=1.2.3\n", &[PropertyAssignment::Literal(8..13)])]
    #[case(b"version=1.2.3\\\n", &[PropertyAssignment::Unsupported])]
    #[case(b"version=1.2.3\\\\\n", &[PropertyAssignment::Literal(8..15)])]
    #[case(b"version=1.2.3\\\\\\\n", &[PropertyAssignment::Unsupported])]
    fn trailing_backslash_assignments_follow_line_continuation_rules(
        #[case] content: &[u8],
        #[case] expected: &[PropertyAssignment],
    ) {
        let assignments = property_assignments(content);

        assert_eq!(assignments, expected);
    }

    #[rstest]
    #[case(b"", true)]
    #[case(b"1.2.3", true)]
    #[case(b"1.2.3\\", false)]
    #[case(b"1.2.3\\\\", true)]
    #[case(b"1.2.3\\\\\\", false)]
    #[case(b"1.2.3\\\\\\\\", true)]
    #[case(b"1.2\\.3\\\\", true)]
    #[case(b"\\", false)]
    #[case(b"\\\\", true)]
    fn property_value_literality_only_depends_on_trailing_backslash_parity(
        #[case] value: &[u8],
        #[case] expected: bool,
    ) {
        assert_eq!(property_value_is_literal(value), expected);
    }

    #[test]
    fn escaped_trailing_backslashes_stay_inside_the_literal_value_range() {
        let content = b"version=1.2.3\\\\\n";

        let assignments = property_assignments(content);

        assert_eq!(assignments, [PropertyAssignment::Literal(8..15)]);
        assert_eq!(&content[8..15], b"1.2.3\\\\");
    }

    #[tokio::test]
    async fn write_gradle_version_rejects_equals_and_whitespace_assignments_as_ambiguous() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"plugins { id(\"java\") }\n";
        let properties_content = b"version=1.0.0\nversion 2.0.0\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        let error = write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Ambiguous active version assignments")
        );
        assert!(error.to_string().contains('2'));
        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            properties_content
        );
    }

    #[tokio::test]
    async fn write_gradle_version_rejects_non_literal_assignment_without_writing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let build_path = temp_dir.path().join("build.gradle.kts");
        let properties_path = temp_dir.path().join("gradle.properties");
        let build_content = b"plugins { id(\"java\") }\n";
        let properties_content = b"version=${releaseVersion}\nreleaseVersion=1.0.0\n";
        tokio::fs::write(&build_path, build_content).await.unwrap();
        tokio::fs::write(&properties_path, properties_content)
            .await
            .unwrap();

        let error = write_gradle_version(&build_path, "1.0.1", GradleVersionScope::ScriptOnly)
            .await
            .unwrap_err();

        let rendered = format!("{error:#}");
        assert!(rendered.contains(
            "The active version assignment is computed, continued, or otherwise non-literal in Gradle properties file"
        ));
        assert!(rendered.contains(&properties_path.display().to_string()));
        assert_eq!(tokio::fs::read(&build_path).await.unwrap(), build_content);
        assert_eq!(
            tokio::fs::read(&properties_path).await.unwrap(),
            properties_content
        );
    }
}
