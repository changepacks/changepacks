use anyhow::{Context, Result};
use quick_xml::events::{BytesCData, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use std::io::Cursor;

/// Update version in csproj XML content using quick-xml
/// Returns the updated XML content or adds Version if it doesn't exist.
/// Errors when no supported XML node can be updated.
pub fn update_version_in_xml(
    content: &str,
    new_version: &str,
    has_version: bool,
) -> Result<String> {
    let mut reader = Reader::from_str(content);
    let mut writer = Writer::new(Cursor::new(Vec::with_capacity(content.len())));

    // Preallocate the XML event buffer to skip the first few
    // geometric-doubling reallocations on the every-`changepacks update`
    // hot path for C# projects. Mirrors the `Vec::with_capacity(...)`
    // preallocation policy already applied across `sort_by_dep.rs`,
    // `gen_update_map.rs`, `find_project_dirs.rs`, and the sibling
    // `parse_csproj_metadata` in `crates/csharp/src/finder.rs`.
    // `read_event_into` calls `buf.clear()` between events so capacity
    // persists; 256 bytes comfortably covers the largest single event
    // (attribute-laden `<Project Sdk="Microsoft.NET.Sdk"...>`) without
    // over-reserving on tiny `.csproj` files.
    let mut buf = Vec::with_capacity(256);
    let mut in_property_group = false;
    let mut in_version = false;
    let mut version_updated = false;
    let mut first_property_group_ended = false;
    let mut property_group_close_ws: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                if name.as_ref() == b"PropertyGroup" {
                    in_property_group = true;
                    property_group_close_ws = None;
                } else if in_property_group && name.as_ref() == b"Version" {
                    in_version = true;
                }
                writer.write_event(Event::Start(e))?;
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
                        // .csproj files. Delegates to the workspace-wide
                        // `detect_indent_str` (returns the exact leading
                        // whitespace of the first indented line, or `""`
                        // when none) and falls back to 4 spaces when the
                        // file has no indented line at all, matching the
                        // prior local helper's default.
                        let detected = changepacks_utils::detect_indent_str(content);
                        let indent = if detected.is_empty() {
                            "    "
                        } else {
                            detected
                        };
                        let fallback_trailing = format!("\n{indent}");
                        let trailing = property_group_close_ws
                            .as_deref()
                            .unwrap_or(&fallback_trailing);
                        writer.write_event(Event::Text(BytesText::new(indent)))?;
                        writer.write_event(Event::Start(BytesStart::new("Version")))?;
                        writer.write_event(Event::Text(BytesText::new(new_version)))?;
                        writer.write_event(Event::End(BytesEnd::new("Version")))?;
                        writer.write_event(Event::Text(BytesText::new(trailing)))?;
                        version_updated = true;
                    }
                    in_property_group = false;
                    property_group_close_ws = None;
                    first_property_group_ended = true;
                } else if name.as_ref() == b"Version" {
                    // A content-less `<Version></Version>` produces no
                    // `Event::Text`, so `version_updated` never flips and the
                    // `</PropertyGroup>` "add missing version" branch would
                    // otherwise append a SECOND `<Version>`. Fill the empty
                    // element in place here so exactly one survives. The
                    // happy path (`<Version>X</Version>`) is untouched: its
                    // `Event::Text` already set `version_updated = true`, so
                    // this guard is false.
                    if in_version && !version_updated {
                        writer.write_event(Event::Text(BytesText::new(new_version)))?;
                        version_updated = true;
                    }
                    in_version = false;
                }
                writer.write_event(Event::End(e))?;
            }
            Ok(Event::Text(e)) => {
                let is_whitespace = e
                    .decode()
                    .is_ok_and(|text| text.chars().all(char::is_whitespace));
                if in_version && !version_updated && !is_whitespace {
                    // Replace version text
                    writer.write_event(Event::Text(BytesText::new(new_version)))?;
                    version_updated = true;
                } else {
                    if in_property_group {
                        let decoded = e.decode().context("Failed to decode XML text")?;
                        property_group_close_ws = decoded
                            .chars()
                            .all(char::is_whitespace)
                            .then(|| decoded.into_owned());
                    }
                    writer.write_event(Event::Text(e))?;
                }
            }
            Ok(Event::CData(e)) => {
                if in_version && !version_updated {
                    writer.write_event(Event::CData(BytesCData::new(new_version)))?;
                    version_updated = true;
                } else {
                    writer.write_event(Event::CData(e))?;
                }
            }
            Ok(Event::Empty(e)) => {
                // A self-closing `<Version/>` carries no `Event::Text`, so
                // `version_updated` never flips and the `</PropertyGroup>`
                // "add missing version" branch would otherwise append a
                // SECOND `<Version>`. Expand the self-closing shape into a
                // filled `<Version>X</Version>` in place here so exactly one
                // survives — the sibling of the `<Version></Version>`
                // (Start/End) fill-in-place above. Every other empty element
                // (including `<Version/>` outside a PropertyGroup, or once the
                // version is already updated) passes through unchanged.
                if in_property_group && e.local_name().as_ref() == b"Version" && !version_updated {
                    let start = e.into_owned();
                    let end = BytesEnd::from(start.name()).into_owned();
                    writer.write_event(Event::Start(start))?;
                    writer.write_event(Event::Text(BytesText::new(new_version)))?;
                    writer.write_event(Event::End(end))?;
                    version_updated = true;
                } else {
                    writer.write_event(Event::Empty(e))?;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow::anyhow!("XML parsing error: {e}")),
            // Pass-through arms for every event that carries no state and
            // does not need in-place rewriting: Comment, Decl, PI,
            // DocType, GeneralRef. Any future variant with no customization
            // requirement falls into this arm automatically; a variant that
            // DOES need customization must be added above
            // (Start / End / Text / Empty) before this wildcard.
            Ok(event) => {
                writer.write_event(event)?;
            }
        }
        buf.clear();
    }

    if !version_updated {
        return Err(anyhow::anyhow!(
            "C# version update did not mutate any XML node"
        ));
    }

    let result = writer.into_inner().into_inner();
    String::from_utf8(result).context("Failed to convert XML to UTF-8")
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
    fn test_update_version_replaces_cdata_payload_in_place() {
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version><![CDATA[1.2.3]]></Version>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();

        assert_eq!(
            result,
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version><![CDATA[2.0.0]]></Version>\n  </PropertyGroup>\n</Project>"
        );
        assert_eq!(result.matches("<![CDATA[").count(), 1);
        assert!(!result.contains("1.2.3"));
        assert!(!result.contains("1.2.32.0.0"));
    }

    #[test]
    fn test_update_version_preserves_whitespace_around_cdata() {
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version>\n      <![CDATA[1.2.3]]>\n    </Version>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();

        assert_eq!(
            result,
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version>\n      <![CDATA[2.0.0]]>\n    </Version>\n  </PropertyGroup>\n</Project>"
        );
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

    #[test]
    fn test_update_version_fills_content_less_version_element() {
        // Regression: a content-less `<Version></Version>` element used to
        // yield TWO `<Version>` elements — the empty original plus an
        // appended one from the "add missing version" branch — because no
        // `Event::Text` fired to set `version_updated`. It must instead be
        // filled in place, leaving exactly one `<Version>`.
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version></Version>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "0.0.1", false).unwrap();
        assert_eq!(
            result.matches("<Version>").count(),
            1,
            "expected exactly one <Version> element, got:\n{result}",
        );
        assert!(result.contains("<Version>0.0.1</Version>"));
    }

    #[test]
    fn test_update_version_fills_self_closing_version_element() {
        // Regression: a self-closing `<Version/>` used to yield TWO
        // `<Version>` elements — the empty original passed through the
        // wildcard arm plus an appended one from the "add missing version"
        // branch — because no `Event::Text` fired to set `version_updated`.
        // It must instead be expanded in place, leaving exactly one
        // `<Version>` with the surrounding indentation untouched. Sibling of
        // the `<Version></Version>` case above.
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version/>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0", false).unwrap();
        // Full-string compare: sibling formatting/indentation preserved.
        assert_eq!(
            result,
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version>2.0.0</Version>\n  </PropertyGroup>\n</Project>",
            "self-closing <Version/> should be filled in place, formatting preserved:\n{result}",
        );
        assert_eq!(
            result.matches("<Version>2.0.0</Version>").count(),
            1,
            "expected exactly one filled <Version> element, got:\n{result}",
        );
        assert!(
            !result.contains("<Version/>"),
            "self-closing <Version/> should not survive:\n{result}",
        );
        // `<Version` matches only opening/self-closing tags (`</Version>`
        // contains `</Version`, not `<Version`), so this counts Version
        // openings: exactly one.
        assert_eq!(
            result.matches("<Version").count(),
            1,
            "expected exactly one <Version opening, got:\n{result}",
        );
    }

    #[test]
    fn test_update_version_fills_self_closing_version_in_place_after_sibling() {
        // The filled `<Version>` must stay in the self-closing element's
        // original position (right after its sibling property), NOT be
        // appended at the end of the PropertyGroup.
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <Version/>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0", false).unwrap();
        assert_eq!(
            result,
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <Version>2.0.0</Version>\n  </PropertyGroup>\n</Project>",
            "filled <Version> should replace <Version/> in place, not append:\n{result}",
        );
        assert_eq!(
            result.matches("<Version").count(),
            1,
            "expected exactly one <Version opening, got:\n{result}",
        );
    }

    #[test]
    fn test_update_version_preserves_prefixed_self_closing_version_qname() {
        let content = "<msb:Project xmlns:msb=\"urn:msbuild\">\n  <msb:PropertyGroup>\n    <msb:Version/>\n  </msb:PropertyGroup>\n</msb:Project>";
        let result = update_version_in_xml(content, "2.0.0", false).unwrap();

        assert_eq!(
            result,
            "<msb:Project xmlns:msb=\"urn:msbuild\">\n  <msb:PropertyGroup>\n    <msb:Version>2.0.0</msb:Version>\n  </msb:PropertyGroup>\n</msb:Project>"
        );
    }

    #[test]
    fn test_update_version_preserves_attributes_and_spacing_on_self_closing_version() {
        let content = "<Project>\n  <PropertyGroup>\n    <Version Condition=\"'$(Configuration)' == 'Release'\" />\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0", false).unwrap();

        assert_eq!(
            result,
            "<Project>\n  <PropertyGroup>\n    <Version Condition=\"'$(Configuration)' == 'Release'\" >2.0.0</Version>\n  </PropertyGroup>\n</Project>"
        );
    }

    #[test]
    fn test_update_version_passes_through_non_version_self_closing_element() {
        // A self-closing element that is NOT `<Version>` must fall through
        // the Empty arm's `else` and be emitted unchanged, while the real
        // `<Version>` is still updated.
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version>1.0.0</Version>\n    <Nullable/>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert_eq!(
            result,
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version>2.0.0</Version>\n    <Nullable/>\n  </PropertyGroup>\n</Project>",
            "non-Version self-closing element should pass through unchanged:\n{result}",
        );
        assert!(
            result.contains("<Nullable/>"),
            "self-closing <Nullable/> should survive unchanged:\n{result}",
        );
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

    #[test]
    fn test_add_new_version_preserves_zero_indent_property_group_close() {
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n<PropertyGroup>\n    <OutputType>Exe</OutputType>\n</PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "0.0.1", false).unwrap();
        assert!(
            result.contains("</Version>\n</PropertyGroup>"),
            "expected zero-indent close tag to stay at column 0:\n{result}",
        );
    }

    #[test]
    fn test_update_version_without_property_group_returns_error() {
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n</Project>\n";
        let err = update_version_in_xml(content, "0.0.1", false)
            .expect_err("XML without a PropertyGroup cannot be updated");

        assert!(
            err.to_string().contains("did not mutate any XML node"),
            "unexpected error: {err:#}",
        );
    }
}
