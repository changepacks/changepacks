use anyhow::{Context, Result};
use quick_xml::events::{BytesCData, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use std::io::Cursor;

struct PropertyGroupContext {
    version_qname: Option<String>,
    close_ws: Option<String>,
    indent: String,
    depth: usize,
    eligible: bool,
}

fn clear_scope_close_ws(
    property_groups: &mut [PropertyGroupContext],
    project_close_ws: &mut Option<String>,
    project_depth: Option<usize>,
    element_depth: usize,
) {
    if let Some(property_group) = property_groups.last_mut() {
        property_group.close_ws = None;
    } else if project_depth == Some(element_depth) {
        *project_close_ws = None;
    }
}

fn has_condition_attribute(element: &BytesStart<'_>) -> Result<bool> {
    for attribute in element.attributes() {
        let attribute = attribute.context("Failed to parse PropertyGroup attribute")?;
        if attribute
            .key
            .local_name()
            .as_ref()
            .eq_ignore_ascii_case(b"Condition")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn is_unconditional_project_property_group(
    element: &BytesStart<'_>,
    element_depth: usize,
    project_depth: Option<usize>,
) -> Result<bool> {
    Ok(
        project_depth.is_some_and(|depth| element_depth == depth + 1)
            && !has_condition_attribute(element)?,
    )
}

fn has_eligible_version(content: &str) -> Result<bool> {
    let mut reader = Reader::from_str(content);
    let mut buf = Vec::with_capacity(256);
    let mut element_depth = 0usize;
    let mut project_depth = None;
    let mut eligible_property_group_depth = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(element)) => {
                element_depth += 1;
                let name = element.local_name();
                if name.as_ref() == b"Project" && project_depth.is_none() {
                    project_depth = Some(element_depth);
                } else if name.as_ref() == b"PropertyGroup"
                    && is_unconditional_project_property_group(
                        &element,
                        element_depth,
                        project_depth,
                    )?
                {
                    eligible_property_group_depth = Some(element_depth);
                } else if name.as_ref() == b"Version"
                    && eligible_property_group_depth.is_some_and(|depth| element_depth == depth + 1)
                {
                    return Ok(true);
                }
            }
            Ok(Event::Empty(element)) => {
                if element.local_name().as_ref() == b"Version"
                    && eligible_property_group_depth.is_some_and(|depth| element_depth == depth)
                {
                    return Ok(true);
                }
            }
            Ok(Event::End(element)) => {
                if element.local_name().as_ref() == b"PropertyGroup"
                    && eligible_property_group_depth == Some(element_depth)
                {
                    eligible_property_group_depth = None;
                }
                element_depth = element_depth
                    .checked_sub(1)
                    .context("unexpected XML end tag")?;
            }
            Ok(Event::Eof) => return Ok(false),
            Err(error) => return Err(anyhow::anyhow!("XML parsing error: {error}")),
            Ok(_) => {}
        }
        buf.clear();
    }
}

fn qname_with_local_name(qname: &str, local_name: &str) -> String {
    qname.rsplit_once(':').map_or_else(
        || local_name.to_owned(),
        |(prefix, _)| format!("{prefix}:{local_name}"),
    )
}

fn trailing_indentation(whitespace: &str) -> &str {
    let start = whitespace
        .as_bytes()
        .iter()
        .rposition(|byte| matches!(byte, b'\n' | b'\r'))
        .map_or(0, |position| position + 1);
    &whitespace[start..]
}

fn contains_line_break(whitespace: &str) -> bool {
    whitespace
        .as_bytes()
        .iter()
        .any(|byte| matches!(byte, b'\n' | b'\r'))
}

/// Update version in csproj XML content using quick-xml
/// Returns the updated XML content or adds Version if it doesn't exist.
/// Errors when no supported XML node can be updated.
pub fn update_version_in_xml(content: &str, new_version: &str) -> Result<String> {
    let has_version = has_eligible_version(content)?;
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
    let detected_indent = changepacks_utils::detect_indent_str(content);
    let indent = if detected_indent.is_empty() {
        "    "
    } else {
        detected_indent
    };
    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else if content.contains('\n') {
        "\n"
    } else if content.contains('\r') {
        "\r"
    } else {
        ""
    };
    let mut property_groups: Vec<PropertyGroupContext> = Vec::new();
    let mut in_version = false;
    let mut version_updated = false;
    let mut element_depth = 0usize;
    let mut project_depth = None;
    let mut project_close_ws: Option<String> = None;
    let mut project_qname: Option<String> = None;
    let mut fallback_group_indent = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                element_depth += 1;
                let name = e.local_name();
                if name.as_ref() == b"Project" && project_depth.is_none() {
                    project_depth = Some(element_depth);
                    let element_name = e.name();
                    let qname = std::str::from_utf8(element_name.as_ref())
                        .context("Failed to decode Project qualified name")?;
                    // Only the raw `Project` qualified name is retained here.
                    // The derived `PropertyGroup`/`Version` names are built at
                    // the single synthesize site below, so the dominant path
                    // (a `.csproj` that already carries `<Version>`) pays no
                    // allocation for names it never writes.
                    project_qname = Some(qname.to_owned());
                }
                let is_top_level = project_depth.is_some_and(|depth| element_depth == depth + 1);
                let preceding_project_ws = if is_top_level {
                    project_close_ws.take()
                } else {
                    None
                };
                if is_top_level && fallback_group_indent.is_none() {
                    fallback_group_indent = Some(
                        preceding_project_ws
                            .as_deref()
                            .map_or("", trailing_indentation)
                            .to_owned(),
                    );
                }
                if let Some(property_group) = property_groups.last_mut() {
                    property_group.close_ws = None;
                }
                if name.as_ref() == b"PropertyGroup" {
                    let group_indent = preceding_project_ws
                        .as_deref()
                        .map_or("", trailing_indentation)
                        .to_owned();
                    let eligible =
                        is_unconditional_project_property_group(&e, element_depth, project_depth)?;
                    if is_top_level && fallback_group_indent.is_none() {
                        fallback_group_indent = Some(group_indent.clone());
                    }
                    let version_qname = if !has_version && !version_updated && eligible {
                        let element_name = e.name();
                        let qname = std::str::from_utf8(element_name.as_ref())
                            .context("Failed to decode PropertyGroup qualified name")?;
                        Some(qname_with_local_name(qname, "Version"))
                    } else {
                        None
                    };
                    property_groups.push(PropertyGroupContext {
                        version_qname,
                        close_ws: None,
                        indent: group_indent,
                        depth: element_depth,
                        eligible,
                    });
                } else if name.as_ref() == b"Version" {
                    in_version = property_groups.last().is_some_and(|property_group| {
                        property_group.eligible && element_depth == property_group.depth + 1
                    });
                }
                writer.write_event(Event::Start(e))?;
            }
            Ok(Event::End(e)) => {
                let name = e.local_name();
                if name.as_ref() == b"PropertyGroup" {
                    if let Some(property_group) = property_groups.pop()
                        && !version_updated
                        && !has_version
                        && let Some(version_qname) = property_group.version_qname
                    {
                        if let Some(trailing) = property_group.close_ws.as_deref() {
                            if contains_line_break(trailing) {
                                writer.write_event(Event::Text(BytesText::new(indent)))?;
                            }
                        } else if !line_ending.is_empty() {
                            let inner_indent = format!("{}{indent}", property_group.indent);
                            writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                            writer.write_event(Event::Text(BytesText::new(&inner_indent)))?;
                        }
                        writer.write_event(Event::Start(BytesStart::new(&version_qname)))?;
                        writer.write_event(Event::Text(BytesText::new(new_version)))?;
                        writer.write_event(Event::End(BytesEnd::new(&version_qname)))?;
                        if let Some(trailing) = property_group.close_ws.as_deref() {
                            writer.write_event(Event::Text(BytesText::new(trailing)))?;
                        } else if !line_ending.is_empty() {
                            writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                            writer
                                .write_event(Event::Text(BytesText::new(&property_group.indent)))?;
                        }
                        version_updated = true;
                    }
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
                } else if name.as_ref() == b"Project"
                    && project_depth == Some(element_depth)
                    && !version_updated
                    && !has_version
                    && let Some(project_qname) = project_qname.as_deref()
                {
                    let property_group_qname =
                        qname_with_local_name(project_qname, "PropertyGroup");
                    let version_qname = qname_with_local_name(project_qname, "Version");
                    let fallback_group_indent = fallback_group_indent.as_deref().unwrap_or("");
                    if let Some(trailing) = project_close_ws.as_deref() {
                        if contains_line_break(trailing) {
                            writer
                                .write_event(Event::Text(BytesText::new(fallback_group_indent)))?;
                        }
                    } else if !line_ending.is_empty() {
                        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                        writer.write_event(Event::Text(BytesText::new(fallback_group_indent)))?;
                    }

                    writer.write_event(Event::Start(BytesStart::new(&property_group_qname)))?;
                    if !line_ending.is_empty() {
                        let inner_indent = format!("{fallback_group_indent}{indent}");
                        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                        writer.write_event(Event::Text(BytesText::new(&inner_indent)))?;
                    }
                    writer.write_event(Event::Start(BytesStart::new(&version_qname)))?;
                    writer.write_event(Event::Text(BytesText::new(new_version)))?;
                    writer.write_event(Event::End(BytesEnd::new(&version_qname)))?;
                    if !line_ending.is_empty() {
                        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                        writer.write_event(Event::Text(BytesText::new(fallback_group_indent)))?;
                    }
                    writer.write_event(Event::End(BytesEnd::new(&property_group_qname)))?;
                    if let Some(trailing) = project_close_ws.as_deref() {
                        writer.write_event(Event::Text(BytesText::new(trailing)))?;
                    } else if !line_ending.is_empty() {
                        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                    }
                    version_updated = true;
                }
                writer.write_event(Event::End(e))?;
                element_depth = element_depth
                    .checked_sub(1)
                    .context("unexpected XML end tag")?;
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
                    if let Some(property_group) = property_groups.last_mut() {
                        let decoded = e.decode().context("Failed to decode XML text")?;
                        property_group.close_ws = decoded
                            .chars()
                            .all(char::is_whitespace)
                            .then(|| decoded.into_owned());
                    } else if project_depth == Some(element_depth) {
                        let decoded = e.decode().context("Failed to decode XML text")?;
                        project_close_ws = decoded
                            .chars()
                            .all(char::is_whitespace)
                            .then(|| decoded.into_owned());
                    }
                    writer.write_event(Event::Text(e))?;
                }
            }
            Ok(Event::CData(e)) => {
                clear_scope_close_ws(
                    &mut property_groups,
                    &mut project_close_ws,
                    project_depth,
                    element_depth,
                );
                if in_version && !version_updated {
                    writer.write_event(Event::CData(BytesCData::new(new_version)))?;
                    version_updated = true;
                } else {
                    writer.write_event(Event::CData(e))?;
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.local_name();
                let is_top_level = project_depth == Some(element_depth);
                let preceding_project_ws = if is_top_level {
                    project_close_ws.take()
                } else {
                    None
                };
                if is_top_level && fallback_group_indent.is_none() {
                    fallback_group_indent = Some(
                        preceding_project_ws
                            .as_deref()
                            .map_or("", trailing_indentation)
                            .to_owned(),
                    );
                }
                if let Some(property_group) = property_groups.last_mut() {
                    property_group.close_ws = None;
                }
                if name.as_ref() == b"Project"
                    && project_depth.is_none()
                    && !version_updated
                    && !has_version
                {
                    let start = e.into_owned();
                    let end = BytesEnd::from(start.name()).into_owned();
                    let element_name = start.name();
                    let project_qname = std::str::from_utf8(element_name.as_ref())
                        .context("Failed to decode Project qualified name")?;
                    let property_group_qname =
                        qname_with_local_name(project_qname, "PropertyGroup");
                    let version_qname = qname_with_local_name(project_qname, "Version");
                    let inner_indent = format!("{indent}{indent}");

                    writer.write_event(Event::Start(start))?;
                    if !line_ending.is_empty() {
                        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                        writer.write_event(Event::Text(BytesText::new(indent)))?;
                    }
                    writer.write_event(Event::Start(BytesStart::new(&property_group_qname)))?;
                    if !line_ending.is_empty() {
                        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                        writer.write_event(Event::Text(BytesText::new(&inner_indent)))?;
                    }
                    writer.write_event(Event::Start(BytesStart::new(&version_qname)))?;
                    writer.write_event(Event::Text(BytesText::new(new_version)))?;
                    writer.write_event(Event::End(BytesEnd::new(&version_qname)))?;
                    if !line_ending.is_empty() {
                        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                        writer.write_event(Event::Text(BytesText::new(indent)))?;
                    }
                    writer.write_event(Event::End(BytesEnd::new(&property_group_qname)))?;
                    if !line_ending.is_empty() {
                        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                    }
                    writer.write_event(Event::End(end))?;
                    version_updated = true;
                } else if name.as_ref() == b"PropertyGroup" && !version_updated && !has_version {
                    let qname = std::str::from_utf8(e.name().as_ref())
                        .context("Failed to decode PropertyGroup qualified name")?
                        .to_owned();
                    let version_qname = qname_with_local_name(&qname, "Version");
                    let group_indent = preceding_project_ws
                        .as_deref()
                        .map_or("", trailing_indentation)
                        .to_owned();
                    let is_unconditional_top_level = is_unconditional_project_property_group(
                        &e,
                        element_depth + 1,
                        project_depth,
                    )?;
                    if is_top_level && fallback_group_indent.is_none() {
                        fallback_group_indent = Some(group_indent.clone());
                    }
                    if is_unconditional_top_level {
                        let start = e.into_owned();
                        let end = BytesEnd::from(start.name()).into_owned();
                        let inner_indent = format!("{group_indent}{indent}");
                        writer.write_event(Event::Start(start))?;
                        if !line_ending.is_empty() {
                            writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                            writer.write_event(Event::Text(BytesText::new(&inner_indent)))?;
                        }
                        writer.write_event(Event::Start(BytesStart::new(&version_qname)))?;
                        writer.write_event(Event::Text(BytesText::new(new_version)))?;
                        writer.write_event(Event::End(BytesEnd::new(&version_qname)))?;
                        if !line_ending.is_empty() {
                            writer.write_event(Event::Text(BytesText::new(line_ending)))?;
                            writer.write_event(Event::Text(BytesText::new(&group_indent)))?;
                        }
                        writer.write_event(Event::End(end))?;
                        version_updated = true;
                    } else {
                        writer.write_event(Event::Empty(e))?;
                    }
                // A self-closing `<Version/>` carries no `Event::Text`, so
                // `version_updated` never flips and the `</PropertyGroup>`
                // "add missing version" branch would otherwise append a
                // SECOND `<Version>`. Expand the self-closing shape into a
                // filled `<Version>X</Version>` in place here so exactly one
                // survives — the sibling of the `<Version></Version>`
                // (Start/End) fill-in-place above. Every other empty element
                // (including `<Version/>` outside a PropertyGroup, or once the
                // version is already updated) passes through unchanged.
                } else if name.as_ref() == b"Version"
                    && !version_updated
                    && property_groups.last().is_some_and(|property_group| {
                        property_group.eligible && element_depth == property_group.depth
                    })
                {
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
                clear_scope_close_ws(
                    &mut property_groups,
                    &mut project_close_ws,
                    project_depth,
                    element_depth,
                );
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

        let result = update_version_in_xml(content, "2.0.0").unwrap();
        assert!(result.contains("<Version>2.0.0</Version>"));
    }

    #[test]
    fn test_update_version_replaces_cdata_payload_in_place() {
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version><![CDATA[1.2.3]]></Version>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0").unwrap();

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
        let result = update_version_in_xml(content, "2.0.0").unwrap();

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

        let result = update_version_in_xml(content, "0.0.1").unwrap();
        assert!(result.contains("<Version>0.0.1</Version>"));
    }

    #[test]
    fn test_add_new_version_uses_first_unconditional_top_level_property_group() {
        let content = "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\n    <Optimize>false</Optimize>\n  </PropertyGroup>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>";
        let expected = "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\n    <Optimize>false</Optimize>\n  </PropertyGroup>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <Version>2.0.0</Version>\n  </PropertyGroup>\n</Project>";

        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_add_new_version_uses_self_closing_unconditional_top_level_property_group() {
        let content = "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\"/>\n  <PropertyGroup/>\n</Project>";
        let expected = "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\"/>\n  <PropertyGroup>\n    <Version>2.0.0</Version>\n  </PropertyGroup>\n</Project>";

        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_add_new_version_ignores_nested_property_group() {
        let content = "<Project>\n  <Target Name=\"Build\">\n    <PropertyGroup>\n      <NestedOnly>true</NestedOnly>\n    </PropertyGroup>\n  </Target>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>";
        let expected = "<Project>\n  <Target Name=\"Build\">\n    <PropertyGroup>\n      <NestedOnly>true</NestedOnly>\n    </PropertyGroup>\n  </Target>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <Version>2.0.0</Version>\n  </PropertyGroup>\n</Project>";

        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_add_new_version_creates_unconditional_group_when_all_groups_are_conditional() {
        let content = "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\n    <Optimize>false</Optimize>\n  </PropertyGroup>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Optimize>true</Optimize>\n  </PropertyGroup>\n</Project>";
        let expected = "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\n    <Optimize>false</Optimize>\n  </PropertyGroup>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Optimize>true</Optimize>\n  </PropertyGroup>\n  <PropertyGroup>\n    <Version>2.0.0</Version>\n  </PropertyGroup>\n</Project>";

        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_add_new_version_preserves_namespaced_qnames() {
        let content = "<msb:Project xmlns:msb=\"urn:msbuild\">\n  <msb:PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\n    <msb:Optimize>false</msb:Optimize>\n  </msb:PropertyGroup>\n</msb:Project>";
        let expected = "<msb:Project xmlns:msb=\"urn:msbuild\">\n  <msb:PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\n    <msb:Optimize>false</msb:Optimize>\n  </msb:PropertyGroup>\n  <msb:PropertyGroup>\n    <msb:Version>2.0.0</msb:Version>\n  </msb:PropertyGroup>\n</msb:Project>";

        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_add_new_version_preserves_crlf_and_space_indentation() {
        let content = "<Project>\r\n    <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\r\n        <Optimize>false</Optimize>\r\n    </PropertyGroup>\r\n</Project>\r\n";
        let expected = "<Project>\r\n    <PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\r\n        <Optimize>false</Optimize>\r\n    </PropertyGroup>\r\n    <PropertyGroup>\r\n        <Version>2.0.0</Version>\r\n    </PropertyGroup>\r\n</Project>\r\n";

        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_add_new_version_preserves_tab_indentation() {
        let content = "<Project>\n\t<PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\n\t\t<Optimize>false</Optimize>\n\t</PropertyGroup>\n</Project>";
        let expected = "<Project>\n\t<PropertyGroup Condition=\"'$(Configuration)' == 'Debug'\">\n\t\t<Optimize>false</Optimize>\n\t</PropertyGroup>\n\t<PropertyGroup>\n\t\t<Version>2.0.0</Version>\n\t</PropertyGroup>\n</Project>";

        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_conditional_version_is_unchanged_when_global_version_is_missing() {
        let content = "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version>1.2.3</Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>";
        let expected = "<Project>\n  <PropertyGroup Condition=\"'$(Configuration)' == 'Release'\">\n    <Version>1.2.3</Version>\n  </PropertyGroup>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <Version>2.0.0</Version>\n  </PropertyGroup>\n</Project>";

        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_update_version_fills_content_less_version_element() {
        // Regression: a content-less `<Version></Version>` element used to
        // yield TWO `<Version>` elements — the empty original plus an
        // appended one from the "add missing version" branch — because no
        // `Event::Text` fired to set `version_updated`. It must instead be
        // filled in place, leaving exactly one `<Version>`.
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <Version></Version>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "0.0.1").unwrap();
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
        let result = update_version_in_xml(content, "2.0.0").unwrap();
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
        let result = update_version_in_xml(content, "2.0.0").unwrap();
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
        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(
            result,
            "<msb:Project xmlns:msb=\"urn:msbuild\">\n  <msb:PropertyGroup>\n    <msb:Version>2.0.0</msb:Version>\n  </msb:PropertyGroup>\n</msb:Project>"
        );
    }

    #[test]
    fn test_update_version_preserves_attributes_and_spacing_on_self_closing_version() {
        let content = "<Project>\n  <PropertyGroup>\n    <Version Condition=\"'$(Configuration)' == 'Release'\" />\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0").unwrap();

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
        let result = update_version_in_xml(content, "2.0.0").unwrap();
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
        let result = update_version_in_xml(content, "2.0.0").unwrap();
        assert!(result.contains("2.0.0"));
        assert!(result.contains(marker));
    }

    #[test]
    fn test_update_version_malformed_xml() {
        let content = r#"<Project><PropertyGroup><Version>1.0.0</Version></PropertyGroup"#;
        let result = update_version_in_xml(content, "2.0.0");
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
        let result = update_version_in_xml(content, "2.0.0");
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
        let result = update_version_in_xml(content, "0.0.1").unwrap();
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
        let result = update_version_in_xml(content, "0.0.1").unwrap();
        assert!(
            result.contains("</Version>\n\t</PropertyGroup>"),
            "expected tab reindent before </PropertyGroup>:\n{result}",
        );
    }

    #[test]
    fn test_add_new_version_preserves_zero_indent_property_group_close() {
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n<PropertyGroup>\n    <OutputType>Exe</OutputType>\n</PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "0.0.1").unwrap();
        assert!(
            result.contains("</Version>\n</PropertyGroup>"),
            "expected zero-indent close tag to stay at column 0:\n{result}",
        );
    }

    #[test]
    fn test_update_version_without_property_group_creates_unconditional_group() {
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n</Project>\n";
        let result = update_version_in_xml(content, "0.0.1").unwrap();

        assert_eq!(
            result,
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n<PropertyGroup>\n    <Version>0.0.1</Version>\n</PropertyGroup>\n</Project>\n"
        );
    }

    #[test]
    fn test_self_closing_project_expands_with_crlf_and_trailing_bytes() -> Result<()> {
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\" />\r\n<!-- trailing -->\r\n";

        let result = update_version_in_xml(content, "1.2.3")?;

        assert_eq!(
            result,
            "<Project Sdk=\"Microsoft.NET.Sdk\" >\r\n    <PropertyGroup>\r\n        <Version>1.2.3</Version>\r\n    </PropertyGroup>\r\n</Project>\r\n<!-- trailing -->\r\n"
        );
        Ok(())
    }

    #[test]
    fn test_namespaced_self_closing_project_preserves_qualified_names() -> Result<()> {
        let content = "<msb:Project xmlns:msb=\"urn:msbuild\"/>\n";

        let result = update_version_in_xml(content, "2.0.0")?;

        assert_eq!(
            result,
            "<msb:Project xmlns:msb=\"urn:msbuild\">\n    <msb:PropertyGroup>\n        <msb:Version>2.0.0</msb:Version>\n    </msb:PropertyGroup>\n</msb:Project>\n"
        );
        Ok(())
    }
}
