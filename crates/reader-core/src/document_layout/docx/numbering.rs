use super::{attribute, value, word_element, xml_error};
use crate::{document_layout::DocxList, Result};
use quick_xml::{
    encoding::Decoder,
    events::{BytesStart, Event},
    NsReader,
};
use std::{array, collections::HashMap};

const LEVELS: usize = 9;
const MAX_MARKER_BYTES: usize = 256;

#[derive(Default)]
pub(super) struct Numbering {
    abstracts: HashMap<u32, Abstract>,
    instances: HashMap<u32, Instance>,
    counters: HashMap<u32, [Option<i64>; LEVELS]>,
}

#[derive(Default)]
struct Instance {
    abstract_id: Option<u32>,
    overrides: [LevelOverride; LEVELS],
}

#[derive(Default)]
struct LevelOverride {
    start: Option<i64>,
    level: Option<Level>,
}

#[derive(Default)]
struct Level {
    start: i64,
    restart: Option<u8>,
    template: Option<String>,
    format: Format,
    style: Option<String>,
    legal: bool,
}

#[derive(Default)]
enum Format {
    #[default]
    Decimal,
    DecimalZero,
    UpperLetter,
    LowerLetter,
    UpperRoman,
    LowerRoman,
    Bullet,
    None,
    Unsupported,
}

impl Level {
    fn read(&mut self, event: &BytesStart<'_>, decoder: Decoder) -> Result<()> {
        match event.local_name().as_ref() {
            b"start" => self.start = value(event, decoder)?.unwrap_or(0),
            b"lvlRestart" => self.restart = value(event, decoder)?,
            b"lvlText" => {
                self.template = attribute(event, decoder, b"val")?
                    .filter(|template| template.len() <= MAX_MARKER_BYTES);
            }
            b"numFmt" => {
                self.format = match attribute(event, decoder, b"val")?.as_deref() {
                    Some("decimal") | None => Format::Decimal,
                    Some("decimalZero") => Format::DecimalZero,
                    Some("upperLetter") => Format::UpperLetter,
                    Some("lowerLetter") => Format::LowerLetter,
                    Some("upperRoman") => Format::UpperRoman,
                    Some("lowerRoman") => Format::LowerRoman,
                    Some("bullet") => Format::Bullet,
                    Some("none") => Format::None,
                    _ => Format::Unsupported,
                }
            }
            b"pStyle" => self.style = attribute(event, decoder, b"val")?,
            b"isLgl" => {
                self.legal = !matches!(
                    attribute(event, decoder, b"val")?.as_deref(),
                    Some("0" | "false" | "off")
                )
            }
            _ => {}
        }
        Ok(())
    }

    fn restart_after(&self, depth: usize) -> Option<usize> {
        match self.restart {
            Some(0) => None,
            Some(level) if usize::from(level) <= depth => Some(usize::from(level - 1)),
            _ => depth.checked_sub(1),
        }
    }
}

type Abstract = [Option<Level>; LEVELS];

impl Instance {
    fn level<'a>(
        &'a self,
        abstract_levels: Option<&'a Abstract>,
        depth: usize,
    ) -> Option<&'a Level> {
        self.overrides[depth]
            .level
            .as_ref()
            .or_else(|| abstract_levels.and_then(|levels| levels[depth].as_ref()))
    }

    fn start(&self, level: &Level, depth: usize) -> i64 {
        self.overrides[depth].start.unwrap_or(level.start)
    }
}

impl Numbering {
    pub(super) fn parse(xml: &str) -> Result<Self> {
        let mut numbering = Self::default();
        let mut reader = NsReader::from_str(xml);
        let mut abstract_id = None;
        let mut instance_id = None;
        let mut override_depth = None;
        let mut level = None;
        loop {
            let decoder = reader.decoder();
            let (namespace, event) = reader.read_resolved_event().map_err(xml_error)?;
            let empty = matches!(event, Event::Empty(_));
            match event {
                Event::Start(ref event) | Event::Empty(ref event)
                    if word_element(&namespace, event.name().as_ref()) =>
                {
                    match event.local_name().as_ref() {
                        b"abstractNum" if !empty => {
                            abstract_id = attribute(event, decoder, b"abstractNumId")?
                                .and_then(|id| id.parse().ok());
                            if let Some(id) = abstract_id {
                                numbering.abstracts.entry(id).or_default();
                            }
                        }
                        b"num" if !empty => {
                            instance_id =
                                attribute(event, decoder, b"numId")?.and_then(|id| id.parse().ok());
                            if let Some(id) = instance_id {
                                numbering.instances.entry(id).or_default();
                            }
                        }
                        b"abstractNumId" => {
                            if let Some(instance) =
                                instance_id.and_then(|id| numbering.instances.get_mut(&id))
                            {
                                instance.abstract_id = value(event, decoder)?;
                            }
                        }
                        b"lvlOverride" if !empty => {
                            override_depth = depth_attribute(event, decoder)?
                        }
                        b"startOverride" => {
                            if let (Some(instance), Some(depth)) = (
                                instance_id.and_then(|id| numbering.instances.get_mut(&id)),
                                override_depth,
                            ) {
                                instance.overrides[depth].start = value(event, decoder)?;
                            }
                        }
                        b"lvl" if !empty => {
                            level = depth_attribute(event, decoder)?
                                .map(|depth| (depth, Level::default()));
                        }
                        _ => {
                            if let Some((_, level)) = &mut level {
                                level.read(event, decoder)?;
                            }
                        }
                    }
                }
                Event::End(event) if word_element(&namespace, event.name().as_ref()) => {
                    match event.local_name().as_ref() {
                        b"abstractNum" => abstract_id = None,
                        b"num" => instance_id = None,
                        b"lvlOverride" => override_depth = None,
                        b"lvl" => {
                            if let Some((depth, level)) = level.take() {
                                if let (Some(instance), Some(override_depth)) = (
                                    instance_id.and_then(|id| numbering.instances.get_mut(&id)),
                                    override_depth,
                                ) {
                                    instance.overrides[override_depth].level = Some(level);
                                } else if let Some(levels) =
                                    abstract_id.and_then(|id| numbering.abstracts.get_mut(&id))
                                {
                                    levels[depth] = Some(level);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(numbering)
    }

    pub(super) fn level_for_style(&self, id: u32, paragraph_style: &str) -> Option<u8> {
        let instance = self.instances.get(&id)?;
        let definitions = instance.abstract_id.and_then(|id| self.abstracts.get(&id));
        (0..LEVELS)
            .find(|&depth| {
                instance
                    .level(definitions, depth)
                    .is_some_and(|level| level.style.as_deref() == Some(paragraph_style))
            })
            .map(|depth| depth as u8)
    }

    pub(super) fn marker(&mut self, list: &DocxList) -> Option<String> {
        let instance = self.instances.get(&list.id)?;
        let abstract_levels = instance.abstract_id.and_then(|id| self.abstracts.get(&id));
        let levels: [Option<&Level>; LEVELS] =
            array::from_fn(|depth| instance.level(abstract_levels, depth));
        let depth = usize::from(list.level);
        let level = levels[depth]?;
        let counters = self.counters.entry(list.id).or_default();
        counters[depth] = Some(counters[depth].map_or_else(
            || instance.start(level, depth),
            |number| number.saturating_add(1),
        ));
        for deeper in depth + 1..LEVELS {
            if levels[deeper].and_then(|level| level.restart_after(deeper)) == Some(depth) {
                counters[deeper] = None;
            }
        }
        if matches!(level.format, Format::None) {
            return None;
        }
        let mut input = level.template.as_ref()?.chars().peekable();
        let mut label = String::new();
        while let Some(ch) = input.next() {
            if ch == '%' {
                if let Some(reference) = input
                    .peek()
                    .and_then(|ch| ch.to_digit(10))
                    .filter(|n| (1..=9).contains(n))
                {
                    input.next();
                    let reference = reference as usize - 1;
                    if reference <= depth {
                        let definition = levels[reference]?;
                        let value = counters[reference]
                            .unwrap_or_else(|| instance.start(definition, reference));
                        let format = if level.legal {
                            &Format::Decimal
                        } else {
                            &definition.format
                        };
                        let numeric = formatted(value, format)?;
                        if label.len() + numeric.len() > MAX_MARKER_BYTES {
                            return None;
                        }
                        label.push_str(&numeric);
                    }
                    continue;
                }
            }
            label.push(
                if matches!(level.format, Format::Bullet) && ch == '\u{f0b7}' {
                    '•'
                } else {
                    ch
                },
            );
        }
        (!label.is_empty() && label.len() <= MAX_MARKER_BYTES).then_some(label)
    }
}

fn depth_attribute(event: &BytesStart<'_>, decoder: Decoder) -> Result<Option<usize>> {
    Ok(attribute(event, decoder, b"ilvl")?
        .and_then(|value| value.parse().ok())
        .filter(|depth| *depth < LEVELS))
}

fn formatted(value: i64, format: &Format) -> Option<String> {
    match format {
        Format::Decimal => Some(value.to_string()),
        Format::DecimalZero => Some(format!("{value:02}")),
        Format::UpperLetter | Format::LowerLetter => {
            let index = usize::try_from(value.checked_sub(1)?).ok()?;
            let count = index / 26 + 1;
            if count > MAX_MARKER_BYTES {
                return None;
            }
            let base = if matches!(format, Format::UpperLetter) {
                b'A'
            } else {
                b'a'
            };
            Some(
                char::from(base + (index % 26) as u8)
                    .to_string()
                    .repeat(count),
            )
        }
        Format::UpperRoman | Format::LowerRoman if (1..=3999).contains(&value) => {
            let text = roman(value);
            Some(if matches!(format, Format::UpperRoman) {
                text
            } else {
                text.to_ascii_lowercase()
            })
        }
        _ => None,
    }
}

fn roman(mut value: i64) -> String {
    const NUMERALS: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut label = String::new();
    for (divisor, symbol) in NUMERALS {
        for _ in 0..value / divisor {
            label.push_str(symbol);
        }
        value %= divisor;
    }
    label
}
