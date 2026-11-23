use super::{plan, Bounds, PageLayout, SourceFragment};
use crate::ocr::{OcrLine, OcrSpan};
use crate::Result;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRow {
    start_utf16: usize,
    end_utf16: usize,
    block_index: Option<usize>,
    pending: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStyle {
    font_family: Option<String>,
    pub(super) font_height: Option<f64>,
    weight: u16,
    italic: bool,
    ink: Option<[f64; 4]>,
    background: [f64; 4],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStyleAnchor {
    start_utf16: usize,
    style: SourceStyle,
}

impl SourceStyle {
    pub(super) fn differs_from(&self, other: &Self) -> bool {
        different_color(self.background, other.background)
            || self
                .ink
                .zip(other.ink)
                .is_some_and(|(a, b)| different_color(a, b))
            || self
                .font_family
                .as_ref()
                .zip(other.font_family.as_ref())
                .is_some_and(|(a, b)| {
                    a != b || self.weight != other.weight || self.italic != other.italic
                })
            || self
                .font_height
                .zip(other.font_height)
                .is_some_and(|(a, b)| a > 0.0 && b > 0.0 && a.max(b) > 1.2 * a.min(b))
    }

    #[cfg(test)]
    pub(super) fn test_style([r, g, b]: [f64; 3], font_height: f64) -> Self {
        Self {
            font_family: None,
            font_height: Some(font_height),
            weight: 400,
            italic: false,
            ink: None,
            background: [r, g, b, 1.0],
        }
    }
}

#[derive(Serialize)]
struct Anchor {
    bounds: Option<Bounds>,
    style: Option<SourceStyle>,
}

#[derive(Serialize)]
pub struct ProjectedLayout {
    #[serde(flatten)]
    layout: PageLayout,
    anchors: Vec<Anchor>,
}

/// Project document UTF-16 ranges onto the original page using its word and line boxes.
pub fn project(
    rows: &[SourceRow],
    geometry: &[OcrLine],
    offset: usize,
    styles: &[SourceStyleAnchor],
) -> Result<ProjectedLayout> {
    let styles = styles
        .iter()
        .map(|anchor| (anchor.start_utf16, &anchor.style))
        .collect();
    let fragments = rows
        .iter()
        .map(|row| project_row(row, geometry, offset, &styles))
        .collect::<Vec<_>>();
    let layout = plan(&fragments)?;
    let anchors = fragments
        .into_iter()
        .map(|fragment| Anchor {
            bounds: fragment.bounds,
            style: fragment.style,
        })
        .collect();
    Ok(ProjectedLayout { layout, anchors })
}

fn project_row(
    row: &SourceRow,
    geometry: &[OcrLine],
    offset: usize,
    styles: &BTreeMap<usize, &SourceStyle>,
) -> SourceFragment {
    let span = row.start_utf16.saturating_sub(offset)..row.end_utf16.saturating_sub(offset);
    let mut fragment = SourceFragment {
        bounds: None,
        line_height: 0.0,
        start_utf16: row.start_utf16,
        block_index: row.block_index,
        pending: row.pending,
        style: None,
    };
    if span.is_empty() {
        return fragment;
    }
    let mut heights = Vec::new();
    let mut include = |bounds: Bounds, span: Option<OcrSpan>| {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }
        fragment.bounds = Some(
            fragment
                .bounds
                .map_or(bounds, |previous| previous.union(bounds)),
        );
        heights.push(bounds.height);
        if let Some(found) = span.and_then(|span| styles.get(&span.start_utf16).copied()) {
            let style = fragment.style.get_or_insert_with(|| found.clone());
            if style.ink.is_none() {
                style.ink = found.ink;
            }
        }
    };
    for line in geometry
        .iter()
        .filter(|line| intersects(&span, line.source_span))
    {
        let mut has_words = false;
        for word in line
            .words
            .iter()
            .filter(|word| intersects(&span, word.source_span))
        {
            has_words = true;
            include(
                Bounds {
                    x: word.x,
                    y: word.y,
                    width: word.width,
                    height: word.height,
                },
                word.source_span,
            );
        }
        if !has_words {
            include(
                Bounds {
                    x: line.x,
                    y: line.y,
                    width: line.width,
                    height: line.height,
                },
                line.source_span,
            );
        }
    }
    if !heights.is_empty() {
        let middle = heights.len() / 2;
        fragment.line_height = *heights.select_nth_unstable_by(middle, f64::total_cmp).1;
    }
    return fragment;

    fn intersects(span: &Range<usize>, bounds: Option<OcrSpan>) -> bool {
        bounds.is_some_and(|bounds| bounds.start_utf16 < span.end && bounds.end_utf16 > span.start)
    }
}

fn different_color(a: [f64; 4], b: [f64; 4]) -> bool {
    (0..3).any(|channel| (a[channel] - b[channel]).abs() > 0.12)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrWord;

    #[test]
    fn projects_page_local_words_and_keeps_heading_font_separate() {
        let span = |start, end| {
            Some(OcrSpan {
                start_utf8: start,
                end_utf8: end,
                start_utf16: start,
                end_utf16: end,
            })
        };
        let line = |start, end, y, words| OcrLine {
            page: 0,
            text: String::new(),
            x: 0.1,
            y,
            width: 0.7,
            height: 0.02,
            confidence: None,
            correction: None,
            block_start: false,
            words,
            source_span: span(start, end),
        };
        let word = OcrWord {
            text: String::new(),
            x: 0.2,
            y: 0.32,
            width: 0.2,
            height: 0.02,
            raw_start_utf16: None,
            source_span: span(5, 10),
        };
        let geometry = [line(0, 4, 0.30, vec![]), line(5, 18, 0.32, vec![word])];
        let rows = [
            SourceRow {
                start_utf16: 100,
                end_utf16: 104,
                block_index: Some(0),
                pending: false,
            },
            SourceRow {
                start_utf16: 105,
                end_utf16: 110,
                block_index: Some(0),
                pending: false,
            },
        ];
        let styles = [
            SourceStyleAnchor {
                start_utf16: 0,
                style: SourceStyle::test_style([1.0; 3], 0.026),
            },
            SourceStyleAnchor {
                start_utf16: 5,
                style: SourceStyle::test_style([1.0; 3], 0.014),
            },
        ];
        let page = project(&rows, &geometry, 100, &styles).unwrap();
        assert_eq!(page.anchors[1].bounds.unwrap().width, 0.2);
        assert_eq!(page.layout.regions.len(), 2);
        assert_eq!(
            project(&rows, &geometry, 100, &[])
                .unwrap()
                .layout
                .regions
                .len(),
            1
        );
    }
}
