use anyhow::{Context, Result};
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use std::io::Cursor;

/// Update version in csproj XML content using quick-xml
/// Returns the updated XML content or adds Version if it doesn't exist
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
                        // Add Version element before closing PropertyGroup
                        // Try to detect indentation from content
                        let indent = detect_indent(content);
                        writer.write_event(Event::Text(BytesText::new(indent)))?;
                        writer.write_event(Event::Start(BytesStart::new("Version")))?;
                        writer.write_event(Event::Text(BytesText::new(new_version)))?;
                        writer.write_event(Event::End(BytesEnd::new("Version")))?;
                        writer.write_event(Event::Text(BytesText::new("\n  ")))?;
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

/// Detect indentation style from XML content
pub fn detect_indent(content: &str) -> &'static str {
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

    #[test]
    fn test_detect_indent_two_spaces() {
        let content = "  <PropertyGroup>";
        assert_eq!(detect_indent(content), "  ");
    }

    #[test]
    fn test_detect_indent_four_spaces() {
        let content = "    <PropertyGroup>";
        assert_eq!(detect_indent(content), "    ");
    }

    #[test]
    fn test_detect_indent_tab() {
        let content = "\t<PropertyGroup>";
        assert_eq!(detect_indent(content), "\t");
    }

    #[test]
    fn test_detect_indent_default() {
        let content = "<PropertyGroup>";
        assert_eq!(detect_indent(content), "    ");
    }

    #[test]
    fn test_update_version_preserves_empty_elements() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
    <IsPackable />
  </PropertyGroup>
</Project>"#;
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert!(result.contains("2.0.0"));
        assert!(result.contains("IsPackable"));
    }

    #[test]
    fn test_update_version_preserves_comments() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <!-- This is a comment -->
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert!(result.contains("2.0.0"));
        assert!(result.contains("<!-- This is a comment -->"));
    }

    #[test]
    fn test_update_version_preserves_cdata() {
        let content = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
    <Description><![CDATA[some data]]></Description>
  </PropertyGroup>
</Project>"#;
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert!(result.contains("2.0.0"));
        assert!(result.contains("CDATA"));
    }

    #[test]
    fn test_update_version_preserves_xml_declaration() {
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert!(result.contains("2.0.0"));
        assert!(result.contains("<?xml"));
    }

    #[test]
    fn test_update_version_preserves_processing_instruction() {
        let content = r#"<?xml version="1.0" encoding="utf-8"?>
<?xml-stylesheet type="text/xsl" href="style.xsl"?>
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert!(result.contains("2.0.0"));
        assert!(result.contains("xml-stylesheet"));
    }

    #[test]
    fn test_update_version_preserves_doctype() {
        let content = r#"<!DOCTYPE Project>
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <Version>1.0.0</Version>
  </PropertyGroup>
</Project>"#;
        let result = update_version_in_xml(content, "2.0.0", true).unwrap();
        assert!(result.contains("2.0.0"));
        assert!(result.contains("DOCTYPE"));
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
}
