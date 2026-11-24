use super::{plan, Bounds, PageLayout, Region, SourceFragment};
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
    pub(super) background: [f64; 4],
    #[serde(default = "mask_safe_default")]
    pub(super) mask_safe: bool,
}

fn mask_safe_default() -> bool { true }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStyleAnchor {
    start_utf16: usize,
    style: SourceStyle,
}

impl SourceStyle {
    pub(super) fn differs_from(&self, other: &Self) -> bool {
        self.mask_safe != other.mask_safe || different_color(self.background, other.background)
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
            mask_safe: true,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Anchor {
    bounds: Option<Bounds>,
    style: Option<SourceStyle>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pending_masks: Vec<super::masks::BackgroundMask>,
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
    preserve_artwork: bool,
) -> Result<ProjectedLayout> {
    let styles = styles
        .iter()
        .map(|anchor| (anchor.start_utf16, &anchor.style))
        .collect();
    let fragments = rows
        .iter()
        .map(|row| project_row(row, geometry, offset, &styles))
        .collect::<Vec<_>>();
    let mut layout = plan(&fragments)?;
    let mut recovered = Vec::new();
    let (regions, overflow) = (&mut layout.regions, &mut layout.overflow);
    regions.retain_mut(|region| {
        let masks_for = |bounds, indices: &[usize]| super::masks::plan(
            bounds, indices.iter().map(|&index| rows[index].page_span(offset)), geometry, &styles,
        );
        let standalone = |&index: &usize| {
            let fragment = &fragments[index];
            let bounds = fragment.bounds?;
            // A multiline sentence can share its rectangle with a neighboring
            // sentence. Neither can safely be drawn on its own in that rectangle.
            if region.fragments.iter().any(|&other| {
                other != index && fragments[other].bounds.is_some_and(|other_bounds| {
                    let (width, height) = bounds.overlap(other_bounds);
                    width > 0.0 && height > 0.0
                })
            }) {
                return None;
            }
            let mut fallback = Region::from_fragment(index, fragment, bounds, region.rotation);
            fallback.alignment = region.alignment;
            fallback.set_font(&fragments);
            if preserve_artwork {
                fallback.masks = masks_for(bounds, &[index])?;
            }
            Some(fallback)
        };
        let fallback: Vec<_> = if region.fragments.len() >= 2 {
            region.fragments.iter().filter_map(standalone).collect()
        } else {
            Vec::new()
        };
        if preserve_artwork {
            let Some(masks) = masks_for(region.bounds, &region.fragments) else {
                let retained: std::collections::HashSet<_> =
                    fallback.iter().map(|standalone| standalone.fragments[0]).collect();
                overflow.extend(region.fragments.iter().copied().filter(|index| !retained.contains(index)));
                recovered.extend(fallback);
                return false;
            };
            region.masks = masks;
        }
        region.fallback_regions = fallback;
        true
    });
    regions.extend(recovered);
    overflow.sort_unstable_by_key(|&index| (rows[index].start_utf16, index));
    let anchors = fragments
        .into_iter()
        .zip(rows)
        .map(|(fragment, row)| {
            // A missing translation may only replace ink we can safely attribute to its own source span.
            let pending_masks = if preserve_artwork && row.pending {
                fragment.bounds.and_then(|bounds| {
                    super::masks::plan(bounds, std::iter::once(row.page_span(offset)), geometry, &styles)
                }).unwrap_or_default()
            } else {
                Vec::new()
            };
            Anchor { bounds: fragment.bounds, style: fragment.style, pending_masks }
        })
        .collect();
    Ok(ProjectedLayout { layout, anchors })
}

impl SourceRow {
    fn page_span(&self, offset: usize) -> Range<usize> {
        self.start_utf16.saturating_sub(offset)..self.end_utf16.saturating_sub(offset)
    }
}

fn project_row(
    row: &SourceRow,
    geometry: &[OcrLine],
    offset: usize,
    styles: &BTreeMap<usize, &SourceStyle>,
) -> SourceFragment {
    let span = row.page_span(offset);
    let mut fragment = SourceFragment {
        bounds: None,
        ink_boxes: Vec::new(),
        line_height: 0.0,
        rotation: Some(0),
        start_utf16: row.start_utf16,
        block_index: row.block_index,
        pending: row.pending,
        style: None,
    };
    if span.is_empty() {
        return fragment;
    }
    let mut heights = Vec::new();
    let mut direction = None;
    let mut mixed = false;
    let mut include = |bounds: Bounds, span: Option<OcrSpan>, rotation: u16| {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }
        fragment.bounds = Some(
            fragment
                .bounds
                .map_or(bounds, |previous| previous.union(bounds)),
        );
        fragment.ink_boxes.push(bounds);
        heights.push(if rotation % 180 == 0 { bounds.height } else { bounds.width });
        if let Some(found) = span.and_then(|span| styles.get(&span.start_utf16).copied()) {
            let style = fragment.style.get_or_insert_with(|| found.clone());
            style.mask_safe &= found.mask_safe;
            if style.ink.is_none() {
                style.ink = found.ink;
            }
        }
    };
    for line in geometry
        .iter()
        .filter(|line| intersects(&span, line.source_span))
    {
        let rotation = crate::ocr::line_rotation(line);
        if let Some(rotation) = rotation {
            mixed |= direction.is_some_and(|previous| previous != rotation);
            direction = Some(rotation);
        }
        let rotation = rotation.unwrap_or_default();
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
                rotation,
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
                rotation,
            );
        }
    }
    fragment.rotation = (!mixed).then_some(direction.unwrap_or_default());
    if !heights.is_empty() {
        let middle = heights.len() / 2;
        fragment.line_height = *heights.select_nth_unstable_by(middle, f64::total_cmp).1;
    }
    return fragment;

    fn intersects(span: &Range<usize>, bounds: Option<OcrSpan>) -> bool {
        bounds.is_some_and(|bounds| bounds.start_utf16 < span.end && bounds.end_utf16 > span.start)
    }
}

pub(super) fn different_color(a: [f64; 4], b: [f64; 4]) -> bool {
    (0..3).any(|channel| (a[channel] - b[channel]).abs() > 0.12)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrWord;

    #[test]
    fn preserves_four_rotations_and_keeps_masks_in_page_coordinates() {
        let entries = [
            (0, (0.1, 0.2), (0.3, 0.2)),
            (90, (0.8, 0.1), (0.8, 0.3)),
            (180, (0.7, 0.8), (0.5, 0.8)),
            (270, (0.2, 0.8), (0.2, 0.5)),
        ];
        let mut rows = Vec::new();
        let mut geometry = Vec::new();
        for (index, (_, (x1, y1), (x2, y2))) in entries.into_iter().enumerate() {
            let start = index * 10;
            let span = |left, right| Some(OcrSpan {
                start_utf8: start + left, end_utf8: start + right,
                start_utf16: start + left, end_utf16: start + right,
            });
            let word = |text: &str, x, y, source_span| OcrWord {
                text: text.into(), x, y, width: 0.03, height: 0.02,
                raw_start_utf16: None, source_span,
            };
            rows.push(SourceRow {
                start_utf16: start, end_utf16: start + 7, block_index: Some(0), pending: false,
            });
            geometry.push(OcrLine {
                rotation: None,
                page: 0, text: "Un mot.".into(), x: f64::min(x1, x2), y: f64::min(y1, y2),
                width: (x2 - x1).abs() + 0.03, height: (y2 - y1).abs() + 0.02,
                confidence: None, correction: None, block_start: false, source_span: span(0, 7),
                words: vec![word("Un", x1, y1, span(0, 2)), word("mot.", x2, y2, span(3, 7))],
            });
        }
        let output = serde_json::to_value(project(&rows, &geometry, 0, &[], true).unwrap()).unwrap();
        let regions = output["regions"].as_array().unwrap();
        assert_eq!(regions.len(), 4);
        for (index, (angle, _, _)) in entries.into_iter().enumerate() {
            assert_eq!(regions[index]["rotation"], angle);
            assert_eq!(regions[index]["masks"].as_array().unwrap().len(), 1);
            assert_eq!(regions[index]["bounds"], output["anchors"][index]["bounds"]);
        }
        rows[0].end_utf16 = rows[1].end_utf16;
        assert!(project(&rows[..1], &geometry, 0, &[], true).unwrap().layout.regions.is_empty());
        geometry[3].words.clear();
        geometry[3].rotation = Some(270);
        let caption = project(&rows[3..], &geometry[3..], 0, &[], true).unwrap();
        assert_eq!(caption.layout.regions[0].rotation, 270);
    }

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
            rotation: None,
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
        let mut rows = [
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
        let page = project(&rows, &geometry, 100, &styles, false).unwrap();
        assert_eq!(page.anchors[1].bounds.unwrap().width, 0.2);
        assert_eq!(page.layout.regions.len(), 2);
        assert_eq!(
            project(&rows, &geometry, 100, &[], false)
                .unwrap()
                .layout
                .regions
                .len(),
            1
        );

        rows[1].pending = true;
        let page = project(&rows, &geometry, 100, &styles, true).unwrap();
        assert_eq!(page.layout.pending, [1]);
        assert!(page.anchors[0].pending_masks.is_empty());
        assert_eq!(page.anchors[1].pending_masks.len(), 1);
        rows[1].end_utf16 = 109;
        let ambiguous = project(&rows, &geometry, 100, &styles, true).unwrap();
        assert!(ambiguous.anchors[1].pending_masks.is_empty());
    }

    #[test]
    fn offers_disjoint_sentence_boxes_with_their_own_masks_when_a_block_overflows() {
        let rows = [0, 8, 16].map(|start| SourceRow {
            start_utf16: start,
            end_utf16: start + 7,
            block_index: Some(0),
            pending: false,
        });
        let mut geometry = std::array::from_fn::<_, 3, _>(|index| {
            let row = &rows[index];
            OcrLine {
                rotation: None,
                page: 0,
                text: "Phrase.".into(),
                x: 0.1,
                y: [0.25, 0.28, 0.31][index],
                width: 0.6,
                height: 0.02,
                confidence: None,
                correction: None,
                block_start: index == 0,
                words: Vec::new(),
                source_span: Some(OcrSpan {
                    start_utf8: row.start_utf16,
                    end_utf8: row.end_utf16,
                    start_utf16: row.start_utf16,
                    end_utf16: row.end_utf16,
                }),
            }
        });
        let draw = |geometry: &[OcrLine], preserve_artwork| {
            let projection = project(&rows, geometry, 0, &[], preserve_artwork).unwrap();
            assert_eq!(projection.layout.regions.len(), 1);
            projection.layout.regions.into_iter().next().unwrap()
        };
        let region = draw(&geometry, true);
        assert_eq!(region.fragments, [0, 1, 2]);
        assert_eq!(region.fallback_regions.len(), 3);
        for (index, fallback) in region.fallback_regions.iter().enumerate() {
            assert_eq!(fallback.fragments, [index]);
            assert_eq!(fallback.masks.len(), 1);
            assert_eq!(fallback.bounds.y, geometry[index].y);
        }
        assert!(draw(&geometry, false).fallback_regions.iter().all(|region| region.masks.is_empty()));

        geometry[2].y = 0.295;
        let overlapping = draw(&geometry, true);
        assert_eq!(overlapping.fallback_regions.len(), 1);
        assert_eq!(overlapping.fallback_regions[0].fragments, [0]);

        geometry[2].y = 0.31;
        let styles = [0, 8, 16].map(|start_utf16| {
            let mut style = SourceStyle::test_style([1.0; 3], 0.016);
            style.mask_safe = start_utf16 != 8;
            SourceStyleAnchor { start_utf16, style }
        });
        let protected = project(&rows, &geometry, 0, &styles, true).unwrap();
        assert_eq!(protected.layout.overflow, [1]);
        assert_eq!(protected.layout.regions.len(), 2);
        assert_eq!(protected.layout.regions[0].fragments, [0]);
        assert_eq!(protected.layout.regions[1].fragments, [2]);

        let mut caption = geometry[1].clone();
        caption.text = "Caption".into();
        caption.source_span = None;
        caption.x = 0.5;
        caption.width = 0.10;
        caption.height = 0.006;
        caption.y = 0.28;
        let mut with_caption = geometry.to_vec();
        with_caption.push(caption);
        let protected = project(&rows, &with_caption, 0, &[], true).unwrap();
        assert_eq!(protected.layout.overflow, [1]);
        assert_eq!(protected.layout.regions.len(), 2);
        assert_eq!(protected.layout.regions[0].fragments, [0]);
        assert_eq!(protected.layout.regions[1].fragments, [2]);
        assert!(protected.layout.regions.iter().all(|region| region.masks.len() == 1));
    }
}
