use std::ops::Range;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PropertyAssignment {
    Literal(Range<usize>),
    Unsupported,
}

const fn is_property_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | 0x0c)
}

fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let preceding_backslashes = bytes[..index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    preceding_backslashes % 2 == 1
}

fn property_value_is_literal(value: &[u8]) -> bool {
    !value.windows(2).any(|window| window == b"${")
        && !value.windows(10).any(|window| window == b"providers.")
        && !value.windows(8).any(|window| window == b"project.")
        && !value.windows(13).any(|window| window == b"findProperty(")
        && !value
            .last()
            .is_some_and(|byte| *byte == b'\\' && is_escaped(value, value.len()))
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
        let mut cursor = line_start;
        while cursor < logical_end && is_property_whitespace(content[cursor]) {
            cursor += 1;
        }

        if !matches!(content.get(cursor), Some(b'#' | b'!'))
            && content.get(cursor..cursor + b"version".len()) == Some(b"version")
        {
            cursor += b"version".len();
            while cursor < logical_end && is_property_whitespace(content[cursor]) {
                cursor += 1;
            }
            if matches!(content.get(cursor), Some(b'=' | b':')) {
                cursor += 1;
                while cursor < logical_end && is_property_whitespace(content[cursor]) {
                    cursor += 1;
                }
                let value_start = cursor;
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

    #[test]
    fn commented_assignments_are_ignored() {
        let content = b"# version=1.0.0\n! version:2.0.0\nother=true\n";

        let assignments = property_assignments(content);

        assert!(assignments.is_empty());
    }
}
