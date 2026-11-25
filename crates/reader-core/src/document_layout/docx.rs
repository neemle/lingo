use super::{materialize_blocks, BlockRange, DocxList, DocxTableCell, Extracted};
use crate::{Result, MAX_IMPORT};
use quick_xml::{
    encoding::Decoder,
    events::{BytesStart, Event},
    name::{QName, ResolveResult},
    NsReader,
};
use std::{
    collections::{HashMap, HashSet},
    io::{Cursor, Read},
};

mod numbering;
use numbering::Numbering;

pub(crate) const DOCX_LAYOUT_VERSION: u8 = 6;
const MARKUP_COMPATIBILITY: &[u8] = b"http://schemas.openxmlformats.org/markup-compatibility/2006";

#[derive(Clone, Default)]
struct ParagraphProperties {
    style: Option<String>,
    outline: Option<u8>,
    list_id: Option<u32>,
    list_level: Option<u8>,
}

impl ParagraphProperties {
    fn read(&mut self, event: &BytesStart<'_>, decoder: Decoder) -> Result<()> {
        match event.local_name().as_ref() {
            b"pStyle" => self.style = attribute(event, decoder, b"val")?,
            b"outlineLvl" => self.outline = value(event, decoder)?.filter(|level| *level <= 9),
            b"numId" => self.list_id = value(event, decoder)?,
            b"ilvl" => self.list_level = value(event, decoder)?.filter(|level| *level < 9),
            _ => {}
        }
        Ok(())
    }
}

struct Style {
    properties: ParagraphProperties,
    base: Option<String>,
}

#[derive(Default)]
struct Styles {
    paragraphs: HashMap<String, Style>,
    default: Option<String>,
}

impl Styles {
    fn resolve(
        &self,
        direct: &ParagraphProperties,
        numbering: &Numbering,
    ) -> (Option<u8>, Option<DocxList>) {
        let mut outline = direct.outline;
        let mut list_id = direct.list_id;
        let mut list_level = direct.list_level;
        let mut style = direct.style.as_deref().or(self.default.as_deref());
        let mut visited = HashSet::new();
        while let Some(id) = style.filter(|id| visited.insert(*id)) {
            let definition = self.paragraphs.get(id);
            if let Some(style) = definition {
                outline = outline.or(style.properties.outline);
                list_id = list_id.or(style.properties.list_id);
                list_level = list_level.or(style.properties.list_level);
            }
            outline = outline.or_else(|| builtin_heading(id));
            style = definition.and_then(|style| style.base.as_deref());
        }
        (
            outline.filter(|level| *level < 9).map(|level| level + 1),
            list_id.filter(|id| *id != 0).map(|id| DocxList {
                id,
                level: direct
                    .list_level
                    .or_else(|| {
                        direct
                            .style
                            .as_deref()
                            .or(self.default.as_deref())
                            .and_then(|style| numbering.level_for_style(id, style))
                    })
                    .or(list_level)
                    .unwrap_or(0),
                marker: None,
            }),
        )
    }
}

// Word uses zero-based outline levels; 9 explicitly means body text.
fn builtin_heading(id: &str) -> Option<u8> {
    if id.eq_ignore_ascii_case("Title") {
        return Some(0);
    }
    id.strip_prefix("Heading")?
        .parse::<u8>()
        .ok()
        .filter(|level| (1..=9).contains(level))
        .map(|level| level - 1)
}

fn parse_styles(xml: &str) -> Result<Styles> {
    let mut reader = NsReader::from_str(xml);
    let mut styles = Styles::default();
    let mut current = None;
    let mut in_properties = false;
    loop {
        let decoder = reader.decoder();
        let (namespace, event) = reader.read_resolved_event().map_err(xml_error)?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(ref event) | Event::Empty(ref event)
                if word_element(&namespace, event.name().as_ref()) =>
            {
                match event.local_name().as_ref() {
                    b"pPrChange" if !empty => {
                        reader.read_to_end(event.name()).map_err(xml_error)?;
                    }
                    b"style" if !empty => {
                        let kind = attribute(event, decoder, b"type")?;
                        let id = attribute(event, decoder, b"styleId")?;
                        current = id
                            .filter(|_| kind.as_deref() == Some("paragraph"))
                            .map(|id| {
                                (
                                    id,
                                    Style {
                                        properties: ParagraphProperties::default(),
                                        base: None,
                                    },
                                )
                            });
                        if let Some((id, _)) = &current {
                            if matches!(
                                attribute(event, decoder, b"default")?.as_deref(),
                                Some("1" | "true" | "on")
                            ) {
                                styles.default = Some(id.clone());
                            }
                        }
                    }
                    b"basedOn" => {
                        if let Some((_, style)) = &mut current {
                            style.base = attribute(event, decoder, b"val")?;
                        }
                    }
                    b"pPr" if !empty => in_properties = true,
                    _ if in_properties => {
                        if let Some((_, style)) = &mut current {
                            style.properties.read(event, decoder)?;
                        }
                    }
                    _ => {}
                }
            }
            Event::End(event) if word_element(&namespace, event.name().as_ref()) => {
                match event.local_name().as_ref() {
                    b"pPr" => in_properties = false,
                    b"style" => {
                        if let Some((id, style)) = current.take() {
                            styles.paragraphs.insert(id, style);
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(styles)
}

#[derive(Default)]
struct TableCursor {
    index: u32,
    row: u32,
    next_row: u32,
    column: u32,
    span: Option<u32>,
}

impl TableCursor {
    fn cell(&self) -> Option<DocxTableCell> {
        Some(DocxTableCell {
            table: self.index,
            row: self.row,
            column: self.column,
            column_span: self.span?,
        })
    }
}

#[derive(Default)]
struct Paragraph {
    properties: ParagraphProperties,
    cell: Option<DocxTableCell>,
    list_emitted: bool,
}

pub(super) fn parse(bytes: &[u8]) -> Result<Extracted> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Некоректний DOCX")?;
    let missing_document = "У DOCX немає текстового документа";
    let document = match relationship_parts(&mut archive, None, ["officeDocument"])? {
        Some([document]) => document.ok_or(missing_document)?,
        None => "word/document.xml".to_owned(),
    };
    let xml = xml_part(&mut archive, &document)?.ok_or(missing_document)?;
    let [styles, numbering] =
        relationship_parts(&mut archive, Some(&document), ["styles", "numbering"])
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                [
                    sibling_part(&document, "styles.xml"),
                    sibling_part(&document, "numbering.xml"),
                ]
                .map(Some)
            });
    let styles = styles
        .and_then(|part| xml_part(&mut archive, &part).ok().flatten())
        .and_then(|xml| parse_styles(&xml).ok())
        .unwrap_or_default();
    let numbering = numbering
        .and_then(|part| xml_part(&mut archive, &part).ok().flatten())
        .and_then(|xml| Numbering::parse(&xml).ok())
        .unwrap_or_default();
    parse_body(&xml, &styles, numbering)
}

fn sibling_part(part: &str, sibling: &str) -> String {
    part.rsplit_once('/')
        .map_or_else(|| sibling.to_owned(), |(dir, _)| format!("{dir}/{sibling}"))
}

fn relationship_parts<const N: usize>(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    part: Option<&str>,
    kinds: [&str; N],
) -> Result<Option<[Option<String>; N]>> {
    let path = part.map_or_else(
        || "_rels/.rels".to_owned(),
        |part| {
            let name = part.rsplit('/').next().unwrap_or(part);
            sibling_part(part, &format!("_rels/{name}.rels"))
        },
    );
    let Some(xml) = xml_part(archive, &path)? else {
        return Ok(None);
    };
    let mut reader = NsReader::from_str(&xml);
    let mut targets = [const { None }; N];
    loop {
        let decoder = reader.decoder();
        let (namespace, event) = reader.read_resolved_event().map_err(xml_error)?;
        let element = match event {
            Event::Start(element) | Event::Empty(element) => element,
            Event::Eof => return Ok(Some(targets)),
            _ => continue,
        };
        if !matches!(namespace, ResolveResult::Bound(uri)
            if uri.as_ref() == b"http://schemas.openxmlformats.org/package/2006/relationships")
            || element.local_name().as_ref() != b"Relationship"
        {
            continue;
        }
        let Some(kind) = attribute(&element, decoder, b"Type")? else {
            continue;
        };
        let Some((prefix, kind)) = kind.rsplit_once('/') else {
            continue;
        };
        if !matches!(
            prefix,
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
                | "http://purl.oclc.org/ooxml/officeDocument/relationships"
        ) {
            continue;
        }
        let Some(index) = kinds.iter().position(|candidate| *candidate == kind) else {
            continue;
        };
        if targets[index].is_some()
            || !matches!(
                attribute(&element, decoder, b"TargetMode")?.as_deref(),
                None | Some("Internal")
            )
        {
            continue;
        }
        targets[index] = attribute(&element, decoder, b"Target")?
            .and_then(|target| internal_part_path(part, &target));
    }
}

fn internal_part_path(source: Option<&str>, target: &str) -> Option<String> {
    if target.is_empty() || target.contains([':', '\\', '?', '#']) {
        return None;
    }
    let (mut path, target) = if let Some(absolute) = target.strip_prefix('/') {
        (Vec::new(), absolute)
    } else {
        let base = source
            .and_then(|part| part.rsplit_once('/'))
            .map(|(dir, _)| dir);
        (
            base.into_iter()
                .flat_map(|dir| dir.split('/'))
                .collect::<Vec<_>>(),
            target,
        )
    };
    for segment in target.split('/') {
        match segment {
            "" => return None,
            "." => {}
            ".." => {
                path.pop()?;
            }
            name => path.push(name),
        }
    }
    (!path.is_empty()).then(|| path.join("/"))
}

fn parse_body(xml: &str, styles: &Styles, mut numbering: Numbering) -> Result<Extracted> {
    let mut reader = NsReader::from_str(xml);
    let mut text = String::new();
    let mut ranges = Vec::new();
    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut tables: Vec<TableCursor> = Vec::new();
    let mut next_table = 0;
    let mut run_depth = 0_usize;
    let mut in_text = false;
    let mut in_properties = false;
    let mut start = 0;
    let mut alternate_selected = Vec::new();
    loop {
        let decoder = reader.decoder();
        let (namespace, event) = reader.read_resolved_event().map_err(xml_error)?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if is_markup_compatibility(&namespace) =>
            {
                match element.local_name().as_ref() {
                    b"AlternateContent" if !empty => alternate_selected.push(false),
                    kind @ (b"Choice" | b"Fallback") => {
                        if let Some(selected) = alternate_selected.last_mut() {
                            let use_branch = !*selected && (kind == b"Fallback"
                                || understands_choice(&reader, element, decoder)?);
                            if use_branch {
                                *selected = true;
                            } else if !empty {
                                reader.read_to_end(element.name()).map_err(xml_error)?;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref element)
                if is_markup_compatibility(&namespace)
                    && element.local_name().as_ref() == b"AlternateContent" =>
            {
                alternate_selected.pop();
            }
            Event::Start(ref event) | Event::Empty(ref event)
                if word_element(&namespace, event.name().as_ref()) =>
            {
                match event.local_name().as_ref() {
                    // Revision wrappers can contain recognizable runs and properties from
                    // the previous version. Insertions and move destinations are read normally.
                    b"del" | b"moveFrom" | b"pPrChange" | b"rPrChange" | b"tblPrChange"
                    | b"tblGridChange" | b"trPrChange" | b"tcPrChange" if !empty => {
                        reader.read_to_end(event.name()).map_err(xml_error)?;
                    }
                    b"p" if !empty => {
                        if let Some(paragraph) = paragraphs.last_mut() {
                            ranges.push(block(
                                start..text.len(),
                                paragraph,
                                styles,
                                &mut numbering,
                            ));
                            start = text.len();
                        }
                        paragraphs.push(Paragraph {
                            cell: tables.last().and_then(TableCursor::cell),
                            ..Paragraph::default()
                        });
                    }
                    b"tbl" if !empty => {
                        tables.push(TableCursor {
                            index: next_table,
                            ..TableCursor::default()
                        });
                        next_table += 1;
                    }
                    b"tr" if !empty => {
                        if let Some(table) = tables.last_mut() {
                            table.row = table.next_row;
                            table.next_row += 1;
                            table.column = 0;
                            table.span = None;
                        }
                    }
                    b"tc" if !empty => {
                        if let Some(table) = tables.last_mut() {
                            table.span = Some(1);
                        }
                    }
                    b"gridBefore" => {
                        if let Some(table) = tables.last_mut() {
                            if table.span.is_none() && table.column == 0 {
                                table.column = value(event, decoder)?.unwrap_or(0);
                            }
                        }
                    }
                    b"gridSpan" => {
                        if let Some(table) = tables.last_mut() {
                            if table.span.is_some() {
                                table.span = Some(
                                    value(event, decoder)?.filter(|span| *span > 0).unwrap_or(1),
                                );
                            }
                        }
                    }
                    b"pPr" if !empty => in_properties = true,
                    b"r" if !empty => run_depth += 1,
                    b"t" if !empty => in_text = true,
                    b"tab" | b"ptab" if run_depth > 0 && !in_properties => text.push('\t'),
                    b"br" | b"cr" if run_depth > 0 && !in_properties => text.push('\n'),
                    b"noBreakHyphen" if run_depth > 0 && !in_properties => text.push('\u{2011}'),
                    _ if in_properties => {
                        if let Some(paragraph) = paragraphs.last_mut() {
                            paragraph.properties.read(event, decoder)?;
                        }
                    }
                    _ => {}
                }
            }
            Event::End(event) if word_element(&namespace, event.name().as_ref()) => {
                match event.local_name().as_ref() {
                    b"p" => {
                        ranges.push(block(
                            start..text.len(),
                            &mut paragraphs.pop().unwrap_or_default(),
                            styles,
                            &mut numbering,
                        ));
                        text.push('\n');
                        start = text.len();
                    }
                    b"tbl" => {
                        tables.pop();
                    }
                    b"tc" => {
                        if let Some(table) = tables.last_mut() {
                            table.column =
                                table.column.saturating_add(table.span.take().unwrap_or(1));
                        }
                    }
                    b"pPr" => in_properties = false,
                    b"r" => run_depth = run_depth.saturating_sub(1),
                    b"t" => in_text = false,
                    _ => {}
                }
            }
            Event::Text(event) if in_text => {
                text.push_str(&event.decode().map_err(xml_error)?);
            }
            Event::GeneralRef(event) if in_text => {
                let reference = event.decode().map_err(xml_error)?;
                if let Some(value) = quick_xml::escape::resolve_predefined_entity(&reference) {
                    text.push_str(value);
                } else if let Some(ch) = reference
                    .strip_prefix("#x")
                    .and_then(|value| u32::from_str_radix(value, 16).ok())
                    .or_else(|| {
                        reference
                            .strip_prefix('#')
                            .and_then(|value| value.parse::<u32>().ok())
                    })
                    .and_then(char::from_u32)
                {
                    text.push(ch);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        if text.len() > MAX_IMPORT {
            return Err("Витягнутий текст завеликий".into());
        }
    }
    if start < text.len() {
        ranges.push(BlockRange {
            offset: start..text.len(),
            ..BlockRange::default()
        });
    }
    let blocks = materialize_blocks(&text, ranges);
    Ok(Extracted {
        text,
        blocks,
        pdf_page_starts: None,
    })
}

fn block(
    offset: std::ops::Range<usize>,
    paragraph: &mut Paragraph,
    styles: &Styles,
    numbering: &mut Numbering,
) -> BlockRange {
    let (heading, mut list) = styles.resolve(&paragraph.properties, numbering);
    if let Some(list) = &mut list {
        if !paragraph.list_emitted {
            list.marker = numbering.marker(list);
            paragraph.list_emitted = true;
        }
    }
    BlockRange {
        offset,
        heading,
        list,
        table_cell: paragraph.cell,
        page: None,
        footnote: false,
    }
}

fn word_element(namespace: &ResolveResult<'_>, name: &[u8]) -> bool {
    word_namespace(namespace) || name.starts_with(b"w:")
}

fn word_namespace(namespace: &ResolveResult<'_>) -> bool {
    matches!(namespace, ResolveResult::Bound(uri)
        if matches!(uri.as_ref(),
            b"http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            | b"http://purl.oclc.org/ooxml/wordprocessingml/main"))
}

fn is_markup_compatibility(namespace: &ResolveResult<'_>) -> bool {
    matches!(namespace, ResolveResult::Bound(uri) if uri.as_ref() == MARKUP_COMPATIBILITY)
}

fn understands_choice(
    reader: &NsReader<&[u8]>,
    element: &BytesStart<'_>,
    decoder: Decoder,
) -> Result<bool> {
    let Some(required) = attribute(element, decoder, b"Requires")? else {
        return Ok(false);
    };
    let mut prefixes = required.split_ascii_whitespace().peekable();
    if prefixes.peek().is_none() {
        return Ok(false);
    }
    Ok(prefixes.all(|prefix| {
        let qualified = format!("{prefix}:capability");
        let (namespace, _) = reader.resolve_element(QName(qualified.as_bytes()));
        word_namespace(&namespace)
    }))
}

fn xml_part(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Option<String>> {
    let member = match archive.by_name(name) {
        Ok(member) => member,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if member.size() > MAX_IMPORT as u64 {
        return Err("Розпакований документ завеликий".into());
    }
    let mut xml = String::new();
    member
        .take(MAX_IMPORT as u64 + 1)
        .read_to_string(&mut xml)
        .map_err(xml_error)?;
    if xml.len() > MAX_IMPORT {
        return Err("Розпакований документ завеликий".into());
    }
    Ok(Some(xml))
}

fn value<T: std::str::FromStr>(event: &BytesStart<'_>, decoder: Decoder) -> Result<Option<T>> {
    Ok(attribute(event, decoder, b"val")?.and_then(|value| value.parse().ok()))
}

fn attribute(event: &BytesStart<'_>, decoder: Decoder, name: &[u8]) -> Result<Option<String>> {
    for attribute in event.attributes() {
        let attribute = attribute.map_err(xml_error)?;
        if attribute.key.local_name().as_ref() == name {
            return attribute
                .decode_and_unescape_value(decoder)
                .map(|value| Some(value.into_owned()))
                .map_err(xml_error);
        }
    }
    Ok(None)
}

fn xml_error(error: impl std::fmt::Display) -> String {
    format!("Некоректний XML: {error}")
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    #[test]
    fn docx_alternate_content_contributes_one_supported_branch() {
        let xml = r#"
            <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                xmlns:compat="http://schemas.openxmlformats.org/markup-compatibility/2006"
                xmlns:future="https://example.invalid/new-drawing" xmlns:alias="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body>
                <compat:AlternateContent>
                  <compat:Choice Requires="future alias">
                    <w:p><w:r><w:t>Unsupported drawing.</w:t></w:r></w:p>
                  </compat:Choice>
                  <compat:Choice Requires="alias">
                    <w:p><w:r><w:t>First supported choice.</w:t></w:r></w:p>
                  </compat:Choice>
                  <compat:Choice Requires="w">
                    <w:p><w:r><w:t>Later supported choice.</w:t></w:r></w:p>
                  </compat:Choice>
                  <compat:Fallback><w:p><w:r><w:t>Unused fallback.</w:t></w:r></w:p></compat:Fallback>
                </compat:AlternateContent>
                <compat:AlternateContent>
                  <compat:Choice Requires="future"><w:p><w:r><w:t>Unsupported only.</w:t></w:r></w:p></compat:Choice>
                  <compat:Fallback><w:p><w:r><w:t>Selected fallback.</w:t></w:r></w:p></compat:Fallback>
                </compat:AlternateContent>
                <w:p><w:r><w:t>Next paragraph.</w:t></w:r></w:p>
              </w:body>
            </w:document>"#;
        let document = super::parse_body(xml, &Default::default(), Default::default()).unwrap();
        assert_eq!(document.text, "First supported choice.\nSelected fallback.\nNext paragraph.\n");
        assert_eq!(document.blocks.len(), 3);
    }

    #[test]
    fn docx_import_uses_linked_body_styles_and_numbering() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, xml) in [
            (
                "_rels/.rels",
                r#"
                <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="https://example.invalid/body.xml" TargetMode="External"/>
                  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="content/book/body.xml"/>
                </Relationships>"#,
            ),
            (
                "content/book/_rels/body.xml.rels",
                r#"
                <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="../layout/styles.xml"/>
                  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="/layout/lists.xml"/>
                </Relationships>"#,
            ),
            (
                "word/document.xml",
                r#"
                <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                  <w:body><w:p><w:r><w:t>Unused part.</w:t></w:r></w:p></w:body>
                </w:document>"#,
            ),
            (
                "content/book/body.xml",
                r#"
                <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                  <w:body>
                    <w:p><w:pPr><w:pStyle w:val="Lesson"/></w:pPr><w:r><w:t>Un chapitre</w:t></w:r></w:p>
                    <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="17"/></w:numPr></w:pPr><w:r><w:t>Une phrase.</w:t></w:r></w:p>
                  </w:body>
                </w:document>"#,
            ),
            (
                "content/layout/styles.xml",
                r#"
                <w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                  <w:style w:type="paragraph" w:styleId="Lesson"><w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style>
                </w:styles>"#,
            ),
            (
                "layout/lists.xml",
                r#"
                <w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                  <w:abstractNum w:abstractNumId="4"><w:lvl w:ilvl="0"><w:start w:val="3"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>
                  <w:num w:numId="17"><w:abstractNumId w:val="4"/></w:num>
                </w:numbering>"#,
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let directory = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open(&directory.path().join("reader.db")).unwrap();
        let document = store.import("lesson.docx", &bytes, "fr").unwrap();
        assert_eq!(document.text, "Un chapitre\nUne phrase.\n");
        assert_eq!(document.blocks[0].heading_level, Some(2));
        let marker = document.blocks[1]
            .list
            .as_ref()
            .and_then(|list| list.marker.as_deref());
        assert_eq!(marker, Some("3."));
    }
}
