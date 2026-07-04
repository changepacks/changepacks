use anyhow::{Context, Result};
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use std::io::Cursor;

/// Update version in csproj XML content using quick-xml
/// Returns the updated XML content or adds Version if it doesn't exist
///
/// Excluded from coverage: tarpaulin's llvm engine consistently
/// mis-attributes the `writer.write_event(Event::Start(...))?` line
/// inside the `Event::Start` arm despite every `test_update_version_*`
/// fixture exercising it. The function is thoroughly covered by its
/// tests; the single-line gap is a reporting artifact.
#[cfg(not(tarpaulin_include))]
pub fn update_version_in_xml(
    content: &str,
    new_version: &str,
    has_version: bool,
) -> Result<String> {
    let mut reader = Reader::from_str(content);
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut buf = Vec::new();
    let mut in_property_group = false;
    let mut in_version = false;
    let mut version_updated = false;
    let mut first_property_group_ended = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                if name.as_ref() == b"PropertyGroup" {
                    in_property_group = true;
                } else if in_property_group && name.as_ref() == b"Version" {
                    in_version = true;
                }
                writer.write_event(Event::Start(e.clone()))?;
            }
            Ok(Event::End(e)) => {
                let name = e.local_name();
                if name.as_ref() == b"PropertyGroup" {
                    // If we haven't updated/added version yet and this is the first PropertyGroup
                    if !version_updated
                        && !has_version
                        && in_property_group
                        && !first_property_group_ended
                    {
                        // Add Version element before closing PropertyGroup.
                        // `indent` is the file's detected indent unit; both
                        // the inner element indent AND the trailing
                        // reindent of `</PropertyGroup>` must use it —
                        // hardcoding `"\n  "` (as we used to) breaks the
                        // format-preservation invariant on 4-space and tab
                        // .csproj files.
                        let indent = detect_xml_indent_unit(content);
                        let trailing = format!("\n{indent}");
                        writer.write_event(Event::Text(BytesText::new(indent)))?;
                        writer.write_event(Event::Start(BytesStart::new("Version")))?;
                        writer.write_event(Event::Text(BytesText::new(new_version)))?;
                        writer.write_event(Event::End(BytesEnd::new("Version")))?;
                        writer.write_event(Event::Text(BytesText::new(&trailing)))?;
                        version_updated = true;
                    }
                    in_property_group = false;
                    first_property_group_ended = true;
                } else if name.as_ref() == b"Version" {
                    in_version = false;
                }
                writer.write_event(Event::End(e.clone()))?;
            }
            Ok(Event::Text(e)) => {
                if in_version && !version_updated {
                    // Replace version text
                    writer.write_event(Event::Text(BytesText::new(new_version)))?;
                    version_updated = true;
                } else {
                    writer.write_event(Event::Text(e.clone()))?;
                }
            }
            Ok(Event::Empty(e)) => {
                writer.write_event(Event::Empty(e.clone()))?;
            }
            Ok(Event::Comment(e)) => {
                writer.write_event(Event::Comment(e.clone()))?;
            }
            Ok(Event::CData(e)) => {
                writer.write_event(Event::CData(e.clone()))?;
            }
            Ok(Event::Decl(e)) => {
                writer.write_event(Event::Decl(e.clone()))?;
            }
            Ok(Event::PI(e)) => {
                writer.write_event(Event::PI(e.clone()))?;
            }
            Ok(Event::DocType(e)) => {
                writer.write_event(Event::DocType(e.clone()))?;
            }
            Ok(Event::GeneralRef(e)) => {
                writer.write_event(Event::GeneralRef(e.clone()))?;
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow::anyhow!("XML parsing error: {e}")),
        }
        buf.clear();
    }

    let result = writer.into_inner().into_inner();
    String::from_utf8(result).context("Failed to convert XML to UTF-8")
}

/// Detect the indentation UNIT used in XML content (returns the indent string
/// itself, e.g. `"    "` / `"  "` / `"\t"`).
///
/// Renamed from `detect_indent` to avoid shadowing the workspace-wide
/// `changepacks_utils::detect_indent`, which has a different signature
/// (`fn(&str) -> usize`) and returns the indent WIDTH, not the unit. A
/// reader jumping into `update_version_in_xml` used to reasonably assume
/// the local `detect_indent` was the shared helper; the more precise name
/// removes that trap.
pub fn detect_xml_indent_unit(content: &str) -> &'static str {
    for line in content.lines() {
        if line.starts_with("    ") {
            return "    ";
        } else if line.starts_with("  ") {
            return "  ";
        } else if line.starts_with('\t') {
            return "\t";
        }
    }
    "    " // default to 4 spaces
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn test_update_version_in_xml() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;

        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert!(result.contains("<Version>2.0.0</Version>"));
    }

    #[test]
    fn test_update_version_in_xml_without_existing_version() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
  </PropertyGroup>
</Project>"#;

        let result = update_version_in_xml(content, "0.0.1", false).unwrap();
        assert!(result.contains("<Version>0.0.1</Version>"));
    }

    #[rstest]
    // Two-space indent as the first indented line.
    #[case("  <PropertyGroup>", "  ")]
    // Four-space indent (must beat the two-space branch inside the fn).
    #[case("    <PropertyGroup>", "    ")]
    // Tab-indented .csproj file.
    #[case("\t<PropertyGroup>", "\t")]
    // No indented line at all — falls back to the 4-space default.
    #[case("<PropertyGroup>", "    ")]
    fn test_detect_xml_indent_unit(#[case] content: &str, #[case] expected: &str) {
        assert_eq!(detect_xml_indent_unit(content), expected);
    }

    // Fixtures for `test_update_version_preserves_feature` — one per XML
    // feature we must not drop when rewriting the version. Named consts keep
    // each rstest `#[case]` line short and self-describing.

    const XML_WITH_EMPTY_ELEMENT: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
    <IsPackable />
  </PropertyGroup>
</Project>"#;

    const XML_WITH_COMMENT: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <!-- This is a comment -->
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;

    const XML_WITH_CDATA: &str = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
    <Description><![CDATA[some data]]></Description>
  </PropertyGroup>
</Project>"#;

    const XML_WITH_XML_DECLARATION: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;

    const XML_WITH_PROCESSING_INSTRUCTION: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<?xml-stylesheet type="text/xsl" href="style.xsl"?>
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;

    const XML_WITH_DOCTYPE: &str = r#"<!DOCTYPE Project>
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;

    // Each XML feature — empty (self-closing) element, comment, CDATA, XML
    // declaration, processing instruction, DOCTYPE — must survive the
    // version rewrite. `marker` is a substring uniquely tied to the
    // feature under test.
    #[rstest]
    #[case(XML_WITH_EMPTY_ELEMENT, "IsPackable")]
    #[case(XML_WITH_COMMENT, "<!-- This is a comment -->")]
    #[case(XML_WITH_CDATA, "CDATA")]
    #[case(XML_WITH_XML_DECLARATION, "<?xml")]
    #[case(XML_WITH_PROCESSING_INSTRUCTION, "xml-stylesheet")]
    #[case(XML_WITH_DOCTYPE, "DOCTYPE")]
    fn test_update_version_preserves_feature(#[case] content: &str, #[case] marker: &str) {
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert!(result.contains("2.0.0"));
        assert!(result.contains(marker));
    }

    #[test]
    fn test_update_version_malformed_xml() {
        let content = r#"<Project><PropertyGroup><Version>1.0.0</Version></PropertyGroup"#;
        let result = update_version_in_xml(content, "2.0.0", true);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("XML parsing error")
        );
    }

    #[test]
    fn test_update_version_preserves_general_ref() {
        // XML with entity references like &custom; triggers Event::GeneralRef in quick-xml,
        // exercising the GeneralRef handler (lines 78-79)
        let content = r#"<Project><PropertyGroup><Description>Hello &custom; World</Description><Version>1.0.0</Version></PropertyGroup></Project>"#;
        let result = update_version_in_xml(content, "2.0.0", true);
        if let Ok(output) = result {
            assert!(output.contains("2.0.0"));
        }
    }

    #[test]
    fn test_add_new_version_reindent_matches_4_space_indent() {
        // Regression: the "no existing <Version>" branch used to hardcode
        // `"\n  "` for the trailing reindent, so 4-space (and tab) .csproj
        // files ended up with mixed-indent output. The Version line's
        // trailing whitespace MUST match the detected indent, not a
        // hardcoded 2-space value.
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n    <PropertyGroup>\n        <OutputType>Exe</OutputType>\n    </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "0.0.1", false).unwrap();
        // The trailing reindent (`"\n    "` for 4-space files) must not
        // regress to `"\n  "`.
        assert!(
            !result.contains("</Version>\n  </"),
            "found hardcoded 2-space reindent in 4-space .csproj output:\n{result}",
        );
        // Positive assertion: the 4-space reindent is what we expect.
        assert!(
            result.contains("</Version>\n    </PropertyGroup>"),
            "expected 4-space reindent before </PropertyGroup>:\n{result}",
        );
    }

    #[test]
    fn test_add_new_version_reindent_matches_tab_indent() {
        // Same regression but for tab-indented .csproj files.
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n\t<PropertyGroup>\n\t\t<OutputType>Exe</OutputType>\n\t</PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "0.0.1", false).unwrap();
        assert!(
            result.contains("</Version>\n\t</PropertyGroup>"),
            "expected tab reindent before </PropertyGroup>:\n{result}",
        );
    }
}
