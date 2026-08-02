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

/// Open an element scope and report the whitespace that preceded it.
///
/// When the element sits directly under `<Project>` the pending
/// `project_close_ws` belongs to it rather than to `</Project>`, so it is taken
/// and, the first time round, seeds `fallback_group_indent` with its trailing
/// indentation. Either way the innermost `PropertyGroup`'s `close_ws` is
/// cleared, because a nested element proves that whitespace was not the run
/// immediately before `</PropertyGroup>`.
///
/// `Event::Start` and `Event::Empty` share this preamble verbatim; only the
/// derivation of `is_top_level` differs, because `element_depth` has already
/// been incremented on the Start path.
fn begin_top_level_scope(
    is_top_level: bool,
    property_groups: &mut [PropertyGroupContext],
    project_close_ws: &mut Option<String>,
    fallback_group_indent: &mut Option<String>,
) -> Option<String> {
    let preceding_project_ws = if is_top_level {
        project_close_ws.take()
    } else {
        None
    };
    if is_top_level && fallback_group_indent.is_none() {
        *fallback_group_indent = Some(
            preceding_project_ws
                .as_deref()
                .map_or("", trailing_indentation)
                .to_owned(),
        );
    }
    if let Some(property_group) = property_groups.last_mut() {
        property_group.close_ws = None;
    }
    preceding_project_ws
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

fn qname_with_local_name(qname: &str, local_name: &str) -> String {
    qname.rsplit_once(':').map_or_else(
        || local_name.to_owned(),
        |(prefix, _)| format!("{prefix}:{local_name}"),
    )
}

/// Emit a complete `<Version>new_version</Version>` element.
///
/// Every synthesis branch in [`rewrite_version`] writes exactly this
/// Start/Text/End triple, so the event order lives here once.
fn write_version_element<W: std::io::Write>(
    writer: &mut Writer<W>,
    version_qname: &str,
    new_version: &str,
) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new(version_qname)))?;
    writer.write_event(Event::Text(BytesText::new(new_version)))?;
    writer.write_event(Event::End(BytesEnd::new(version_qname)))?;
    Ok(())
}

/// Break the line and re-indent before the next synthesized element.
///
/// A terminator-free document has an empty `line_ending`; it must stay on one
/// line, so nothing is written at all — not even the indent.
fn write_line_break_and_indent<W: std::io::Write>(
    writer: &mut Writer<W>,
    line_ending: &str,
    indent: &str,
) -> Result<()> {
    if line_ending.is_empty() {
        return Ok(());
    }
    writer.write_event(Event::Text(BytesText::new(line_ending)))?;
    writer.write_event(Event::Text(BytesText::new(indent)))?;
    Ok(())
}

/// Break the line and indent one level deeper than `group_indent`.
///
/// The callers that open a `PropertyGroup` all need the same
/// `group_indent + indent` column for the `<Version>` they are about to write.
/// The two indent segments are emitted as two consecutive text events, which is
/// byte-identical to emitting their concatenation. The empty-`line_ending`
/// check is repeated here rather than left to [`write_line_break_and_indent`]
/// so the deeper `indent` is suppressed too for a document that must stay on
/// one line.
fn write_nested_line_break_and_indent<W: std::io::Write>(
    writer: &mut Writer<W>,
    line_ending: &str,
    group_indent: &str,
    indent: &str,
) -> Result<()> {
    if line_ending.is_empty() {
        return Ok(());
    }
    write_line_break_and_indent(writer, line_ending, group_indent)?;
    writer.write_event(Event::Text(BytesText::new(indent)))?;
    Ok(())
}

/// Insert a synthesized `<Version>` immediately before the `</PropertyGroup>`
/// that is closing now.
///
/// `close_ws` is the whitespace run that directly preceded the close tag, when
/// the group ended with one. That run already carries the group's own
/// indentation, so it is replayed verbatim after the new element (keeping
/// `</PropertyGroup>` in its original column) and the new element only needs
/// one further level of `indent` in front of it. A group that had no such run
/// was written inline, so both sides are synthesized from `line_ending` plus
/// `group_indent` instead.
fn write_version_before_property_group_end<W: std::io::Write>(
    writer: &mut Writer<W>,
    version_qname: &str,
    new_version: &str,
    close_ws: Option<&str>,
    group_indent: &str,
    line_ending: &str,
    indent: &str,
) -> Result<()> {
    if let Some(trailing) = close_ws {
        if contains_line_break(trailing) {
            writer.write_event(Event::Text(BytesText::new(indent)))?;
        }
    } else {
        write_nested_line_break_and_indent(writer, line_ending, group_indent, indent)?;
    }
    write_version_element(writer, version_qname, new_version)?;
    if let Some(trailing) = close_ws {
        writer.write_event(Event::Text(BytesText::new(trailing)))?;
    } else {
        write_line_break_and_indent(writer, line_ending, group_indent)?;
    }
    Ok(())
}

/// Synthesize a whole `<PropertyGroup><Version>...</Version></PropertyGroup>`
/// immediately before the `</Project>` that is closing now, for a document that
/// offered no eligible group to insert into.
///
/// Both new qualified names inherit `project_qname`'s prefix. `group_indent` is
/// the column the document already uses for `<Project>`'s children, and
/// `project_close_ws` is the whitespace run that directly preceded
/// `</Project>`: when it exists it is replayed verbatim after the new group, so
/// only the indent ahead of the group has to be re-emitted, and only when that
/// run actually broke the line.
fn write_property_group_before_project_end<W: std::io::Write>(
    writer: &mut Writer<W>,
    project_qname: &str,
    new_version: &str,
    project_close_ws: Option<&str>,
    group_indent: &str,
    line_ending: &str,
    indent: &str,
) -> Result<()> {
    let property_group_qname = qname_with_local_name(project_qname, "PropertyGroup");
    let version_qname = qname_with_local_name(project_qname, "Version");
    if let Some(trailing) = project_close_ws {
        if contains_line_break(trailing) {
            writer.write_event(Event::Text(BytesText::new(group_indent)))?;
        }
    } else {
        write_line_break_and_indent(writer, line_ending, group_indent)?;
    }

    writer.write_event(Event::Start(BytesStart::new(&property_group_qname)))?;
    write_nested_line_break_and_indent(writer, line_ending, group_indent, indent)?;
    write_version_element(writer, &version_qname, new_version)?;
    write_line_break_and_indent(writer, line_ending, group_indent)?;
    writer.write_event(Event::End(BytesEnd::new(&property_group_qname)))?;
    if let Some(trailing) = project_close_ws {
        writer.write_event(Event::Text(BytesText::new(trailing)))?;
    } else if !line_ending.is_empty() {
        writer.write_event(Event::Text(BytesText::new(line_ending)))?;
    }
    Ok(())
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

/// Render `content` with `new_version` applied and report whether any XML node
/// was actually mutated.
///
/// `has_version` claims that the document already carries an eligible
/// `<Version>` element. It gates *only* the four synthesis branches (adding a
/// `<Version>` to a `PropertyGroup`, adding a whole `PropertyGroup` to
/// `<Project>`, and the two self-closing counterparts); the branches that
/// rewrite an existing `<Version>` in place are never gated by it. A
/// `has_version = true` pass therefore flips `version_updated` exactly when an
/// eligible `<Version>` exists, which is what lets [`update_version_in_xml`]
/// use it as the eligibility probe instead of running a separate detection
/// parse over the same document.
fn rewrite_version(content: &str, new_version: &str, has_version: bool) -> Result<(String, bool)> {
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
    // Probe order encodes a whole-file precedence, not a first-terminator
    // one: CRLF anywhere outranks a lone LF that appears earlier. Each
    // `str::contains` is memchr/SIMD-accelerated, so this ladder beats a
    // hand-rolled single-pass byte scan even in its worst case (a
    // terminator-free file, which pays all three probes); see
    // `test_add_new_version_prefers_crlf_when_an_lf_appears_first` for the
    // precedence guard.
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
                let preceding_project_ws = begin_top_level_scope(
                    is_top_level,
                    &mut property_groups,
                    &mut project_close_ws,
                    &mut fallback_group_indent,
                );
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
                        write_version_before_property_group_end(
                            &mut writer,
                            &version_qname,
                            new_version,
                            property_group.close_ws.as_deref(),
                            &property_group.indent,
                            line_ending,
                            indent,
                        )?;
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
                    write_property_group_before_project_end(
                        &mut writer,
                        project_qname,
                        new_version,
                        project_close_ws.as_deref(),
                        fallback_group_indent.as_deref().unwrap_or(""),
                        line_ending,
                        indent,
                    )?;
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
                let preceding_project_ws = begin_top_level_scope(
                    is_top_level,
                    &mut property_groups,
                    &mut project_close_ws,
                    &mut fallback_group_indent,
                );
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
                    write_line_break_and_indent(&mut writer, line_ending, indent)?;
                    writer.write_event(Event::Start(BytesStart::new(&property_group_qname)))?;
                    write_line_break_and_indent(&mut writer, line_ending, &inner_indent)?;
                    write_version_element(&mut writer, &version_qname, new_version)?;
                    write_line_break_and_indent(&mut writer, line_ending, indent)?;
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
                        writer.write_event(Event::Start(start))?;
                        write_nested_line_break_and_indent(
                            &mut writer,
                            line_ending,
                            &group_indent,
                            indent,
                        )?;
                        write_version_element(&mut writer, &version_qname, new_version)?;
                        write_line_break_and_indent(&mut writer, line_ending, &group_indent)?;
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

    let result = writer.into_inner().into_inner();
    let rendered = String::from_utf8(result).context("Failed to convert XML to UTF-8")?;
    Ok((rendered, version_updated))
}

/// Update version in csproj XML content using quick-xml
/// Returns the updated XML content or adds Version if it doesn't exist.
/// Errors when no supported XML node can be updated.
pub fn update_version_in_xml(content: &str, new_version: &str) -> Result<String> {
    // First pass doubles as the eligibility probe: with `has_version = true`
    // every synthesis branch is disabled, so the pass mutates a node exactly
    // when the document already holds an eligible `<Version>` — and when it
    // does, its output is already the final answer. Only a document that needs
    // a `<Version>` synthesized pays a second parse.
    let (rendered, version_updated) = rewrite_version(content, new_version, true)?;
    if version_updated {
        return Ok(rendered);
    }

    let (rendered, version_updated) = rewrite_version(content, new_version, false)?;
    if version_updated {
        return Ok(rendered);
    }

    Err(anyhow::anyhow!(
        "C# version update did not mutate any XML node"
    ))
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
    fn test_update_version_preserves_attributes_on_filled_version_element() {
        // A `<Version>` written as Start/Text/End while carrying attributes is
        // the shape where a naive rewrite (re-synthesizing the tag from its
        // local name, the way the two "add missing version" branches do)
        // would silently DROP the attributes. The Start arm must instead pass
        // the original element through verbatim and only the Text arm may
        // change, so the `Condition` attribute survives byte-for-byte.
        // Sibling of the self-closing attribute case below.
        let content = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <Version Condition=\"'$(Configuration)' == 'Release'\">1.0.0</Version>\n  </PropertyGroup>\n</Project>";
        let result = update_version_in_xml(content, "2.0.0").unwrap();

        assert_eq!(
            result,
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <Version Condition=\"'$(Configuration)' == 'Release'\">2.0.0</Version>\n  </PropertyGroup>\n</Project>",
            "attribute-bearing <Version> should keep its attributes and only swap its text:\n{result}",
        );
        assert!(
            result.contains("Condition=\"'$(Configuration)' == 'Release'\""),
            "Condition attribute must survive byte-for-byte:\n{result}",
        );
        // `<Version` matches only opening tags (`</Version>` contains
        // `</Version`, not `<Version`), so exactly one opening proves the
        // `</PropertyGroup>` insert branch did not also fire and append a
        // second, attribute-less `<Version>`.
        assert_eq!(
            result.matches("<Version").count(),
            1,
            "expected exactly one <Version opening, got:\n{result}",
        );
        assert!(!result.contains("1.0.0"), "old version survived:\n{result}");
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
        let content = r"<Project><PropertyGroup><Version>1.0.0</Version></PropertyGroup";
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
    fn test_property_group_malformed_attribute_returns_contextual_error() {
        // A `PropertyGroup` start tag carrying a valueless attribute makes
        // quick-xml's attribute iterator yield `Err`. That failure must
        // surface as the contextual `Failed to parse PropertyGroup
        // attribute` error rather than a bare `AttrError`, mirroring the
        // `ProjectReference` counterpart pinned in `finder.rs`.
        let content =
            r"<Project><PropertyGroup Broken><Version>1.0.0</Version></PropertyGroup></Project>";

        let error = update_version_in_xml(content, "2.0.0").unwrap_err();

        assert!(
            format!("{error:#}").contains("Failed to parse PropertyGroup attribute"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn test_update_version_preserves_general_ref() {
        // XML with entity references like &custom; triggers Event::GeneralRef in quick-xml,
        // exercising the GeneralRef handler (lines 78-79)
        let content = r"<Project><PropertyGroup><Description>Hello &custom; World</Description><Version>1.0.0</Version></PropertyGroup></Project>";
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

    #[test]
    fn test_add_new_version_uses_bare_carriage_return_line_ending() -> Result<()> {
        // Classic-Mac `.csproj`: the only terminator is a bare `\r`, so the
        // `line_ending` ladder must resolve to `"\r"` and NEVER synthesize an
        // LF. `<PropertyGroup>` closes with no preceding whitespace text, so
        // `close_ws` stays `None` and the insertion genuinely goes through the
        // `line_ending` branch instead of reusing captured trailing
        // whitespace. `detect_indent_str` splits on CR as well as LF, so this
        // CR-only file reports its real 2-space indent and the `"    "`
        // default does NOT apply: the synthesized inner indent is the
        // 2-space `<PropertyGroup>` indent plus the 2-space unit.
        let content =
            "<Project>\r  <PropertyGroup><OutputType>Exe</OutputType></PropertyGroup>\r</Project>";

        let result = update_version_in_xml(content, "2.0.0")?;

        assert_eq!(
            result,
            "<Project>\r  <PropertyGroup><OutputType>Exe</OutputType>\r    <Version>2.0.0</Version>\r  </PropertyGroup>\r</Project>"
        );
        assert!(
            !result.contains('\n'),
            "no line feed may be introduced into a CR-only file:\n{result:?}",
        );
        Ok(())
    }

    #[test]
    fn test_add_new_version_prefers_crlf_when_an_lf_appears_first() -> Result<()> {
        // Mixed-terminator `.csproj`: a lone LF precedes the first CRLF. The
        // contract is "CRLF anywhere wins", NOT "first terminator wins", so
        // the synthesized `<Version>` must be joined with `"\r\n"`. This pins
        // the whole-file precedence that a first-terminator scan would break,
        // silently rewriting the bytes of users' project files.
        let content = "<Project>\n  <PropertyGroup><OutputType>Exe</OutputType></PropertyGroup>\r\n</Project>\r\n";

        let result = update_version_in_xml(content, "2.0.0")?;

        assert_eq!(
            result,
            "<Project>\n  <PropertyGroup><OutputType>Exe</OutputType>\r\n    <Version>2.0.0</Version>\r\n  </PropertyGroup>\r\n</Project>\r\n"
        );
        Ok(())
    }

    #[test]
    fn test_add_new_version_omits_line_ending_for_terminator_free_content() -> Result<()> {
        // A single-line `.csproj` with no terminator at all resolves the
        // `line_ending` ladder to `""`; every `!line_ending.is_empty()` guard
        // must then stay silent so the output remains a single line rather
        // than gaining an invented terminator.
        let content = "<Project/>";

        let result = update_version_in_xml(content, "2.0.0")?;

        assert_eq!(
            result,
            "<Project><PropertyGroup><Version>2.0.0</Version></PropertyGroup></Project>"
        );
        Ok(())
    }
}
