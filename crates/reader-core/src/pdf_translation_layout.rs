use crate::Result;
use serde::{Deserialize, Serialize};

mod masks;
mod source;
pub mod text;
pub use source::{project, SourceRow, SourceStyle, SourceStyleAnchor};

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Bounds {
    fn right(self) -> f64 {
        self.x + self.width
    }
    fn bottom(self) -> f64 {
        self.y + self.height
    }

    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: self.right().max(other.right()) - x,
            height: self.bottom().max(other.bottom()) - y,
        }
    }

    fn overlap(self, other: Self) -> (f64, f64) {
        (
            self.right().min(other.right()) - self.x.max(other.x),
            self.bottom().min(other.bottom()) - self.y.max(other.y),
        )
    }

    fn intersects(self, other: Self) -> bool {
        let (width, height) = self.overlap(other);
        width > 0.001 && height > 0.001
    }

    fn reading_box(self, rotation: u16) -> Self {
        let (x, y, width, height) = crate::ocr::reading_box((self.x, self.y, self.width, self.height), rotation);
        Self { x, y, width, height }
    }
}

struct SourceFragment {
    bounds: Option<Bounds>,
    ink_boxes: Vec<Bounds>,
    line_height: f64,
    rotation: Option<u16>,
    start_utf16: usize,
    block_index: Option<usize>,
    pending: bool,
    style: Option<SourceStyle>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    bounds: Bounds,
    #[serde(skip_serializing_if = "Option::is_none")]
    wider_bounds: Option<Bounds>,
    alignment: Alignment,
    rotation: u16,
    line_height: f64,
    preferred_font_height: f64,
    limit_font_by_width: bool,
    fragments: Vec<usize>,
    style_fragment: Option<usize>,
    masks: Vec<masks::BackgroundMask>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fallback_regions: Vec<Region>,
}

impl Region {
    fn from_fragment(index: usize, fragment: &SourceFragment, bounds: Bounds, rotation: u16) -> Self {
        Self {
            bounds,
            wider_bounds: None,
            alignment: Alignment::Left,
            rotation,
            line_height: fragment.line_height,
            preferred_font_height: 0.0,
            limit_font_by_width: false,
            fragments: vec![index],
            style_fragment: fragment.style.as_ref().map(|_| index),
            masks: Vec::new(),
            fallback_regions: Vec::new(),
        }
    }

    fn set_font(&mut self, fragments: &[SourceFragment]) {
        let font = self
            .style_fragment
            .and_then(|index| fragments[index].style.as_ref()?.font_height)
            .filter(|height| *height > 0.0 && self.rotation % 180 == 0);
        self.preferred_font_height = font.map_or(self.line_height * 0.82, |height| {
            height.min(self.line_height * 0.90)
        });
        self.limit_font_by_width = font.is_none();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Alignment {
    Left,
    Center,
    Right,
}

#[derive(Default, Serialize)]
pub struct PageLayout {
    regions: Vec<Region>,
    pending: Vec<usize>,
    overflow: Vec<usize>,
}

/// Plan in normalized page coordinates, so resizing the viewport never changes block membership.
fn plan(fragments: &[SourceFragment]) -> Result<PageLayout> {
    let mut page = PageLayout::default();
    for (index, fragment) in fragments.iter().enumerate() {
        let Some(bounds) = fragment.bounds else {
            page.overflow.push(index);
            continue;
        };
        if fragment.pending {
            page.pending.push(index);
            continue;
        }
        if fragments.len() > 4096 {
            page.overflow.push(index);
            continue;
        }
        let Some(rotation) = fragment.rotation else {
            page.overflow.push(index);
            continue;
        };
        let mut region = Region::from_fragment(index, fragment, bounds, rotation);
        while let Some(position) = page
            .regions
            .iter()
            .rposition(|previous| fits(previous, &region, fragment.block_index, fragments))
        {
            let previous_bounds = page.regions[position].bounds;
            let expanded = region.bounds.union(previous_bounds);
            // The space between two lines can belong to another column or caption.
            // Check source boxes too, including those that have yet to be planned.
            if fragments.iter().any(|other| {
                (other.pending || other.block_index != fragment.block_index) &&
                    other.bounds.is_some_and(|bounds| expanded.intersects(bounds) &&
                        !region.bounds.intersects(bounds) && !previous_bounds.intersects(bounds))
            }) {
                break;
            }
            let previous = page.regions.remove(position);
            region.bounds = expanded;
            region.line_height = region.line_height.min(previous.line_height);
            region.fragments.extend(previous.fragments);
            region.style_fragment = [region.style_fragment, previous.style_fragment]
                .into_iter()
                .flatten()
                .min_by_key(|&index| fragments[index].start_utf16);
        }
        region
            .fragments
            .sort_unstable_by_key(|&index| (fragments[index].start_utf16, index));
        region.set_font(fragments);
        page.regions.push(region);
    }
    resolve_overlapping_regions(&mut page, fragments);
    for region in &mut page.regions {
        region.alignment = infer_alignment(
            region.fragments.iter().flat_map(|&index| fragments[index].ink_boxes.iter()
                .map(|bounds| bounds.reading_box(region.rotation))),
            region.line_height,
        );
    }
    page.overflow
        .sort_unstable_by_key(|&index| (fragments[index].start_utf16, index));
    Ok(page)
}

fn infer_alignment(boxes: impl Iterator<Item = Bounds>, line_height: f64) -> Alignment {
    let center = |bounds: Bounds| bounds.x + bounds.width * 0.5;
    let baseline = |bounds: Bounds| bounds.y + bounds.height * 0.5;
    let mut boxes: Vec<_> = boxes.filter(|bounds| bounds.height >= line_height * 0.55).collect();
    boxes.sort_unstable_by(|a, b| baseline(*a).total_cmp(&baseline(*b)));
    let mut lines: Vec<Bounds> = Vec::new();
    for bounds in boxes {
        if let Some(line) = lines.last_mut().filter(|line| {
            (baseline(**line) - baseline(bounds)).abs() <= 0.45 * line_height
        }) {
            *line = line.union(bounds);
        } else {
            lines.push(bounds);
        }
    }
    let mut lines = lines.into_iter().filter(|line| line.width >= 1.5 * line_height);
    let Some(first) = lines.next() else { return Alignment::Left; };
    let (mut left, mut right, mut middle) = (
        (first.x, first.x),
        (first.right(), first.right()),
        (center(first), center(first)),
    );
    for line in lines {
        left = (left.0.min(line.x), left.1.max(line.x));
        right = (right.0.min(line.right()), right.1.max(line.right()));
        middle = (middle.0.min(center(line)), middle.1.max(center(line)));
    }
    let tolerance = (line_height * 0.25).max(0.004);
    let (left_drift, right_drift, center_drift) = (left.1 - left.0, right.1 - right.0, middle.1 - middle.0);
    if left_drift <= tolerance { return Alignment::Left; }
    if right_drift <= tolerance && left_drift >= 2.0 * tolerance { return Alignment::Right; }
    if right_drift > tolerance && center_drift <= tolerance &&
        center_drift <= 0.5 * left_drift.min(right_drift)
    {
        return Alignment::Center;
    }
    Alignment::Left
}

fn resolve_overlapping_regions(page: &mut PageLayout, fragments: &[SourceFragment]) {
    let mut colliding: Vec<_> = page.regions.iter().map(|region| {
        page.pending.iter().filter_map(|&index| fragments[index].bounds)
            .any(|bounds| region.bounds.intersects(bounds))
    }).collect();
    for (index, region) in page.regions.iter().enumerate() {
        for (other, candidate) in page.regions.iter().enumerate().skip(index + 1) {
            if region.bounds.intersects(candidate.bounds) {
                colliding[index] = true;
                colliding[other] = true;
            }
        }
    }
    let mut colliding = colliding.into_iter();
    let mut recovered = Vec::new();
    page.regions.retain_mut(|region| {
        if colliding.next() != Some(true) {
            return true;
        }
        // A paragraph rectangle can collide even when some of its individual
        // sentence boxes are clear. Keep those translations on the page.
        for &index in &region.fragments {
            let fragment = &fragments[index];
            let Some(bounds) = fragment.bounds.filter(|bounds| {
                !fragments.iter().enumerate().any(|(other, fragment)| {
                    other != index && fragment.bounds.is_some_and(|area| bounds.intersects(area))
                })
            }) else {
                page.overflow.push(index);
                continue;
            };
            let mut standalone = Region::from_fragment(index, fragment, bounds, region.rotation);
            standalone.set_font(fragments);
            recovered.push(standalone);
        }
        false
    });
    page.regions.extend(recovered);
}

fn fits(
    previous: &Region,
    region: &Region,
    block: Option<usize>,
    fragments: &[SourceFragment],
) -> bool {
    if previous.rotation != region.rotation || (region.rotation != 0 && block.is_none()) {
        return false;
    }
    if fragments[previous.fragments[0]].block_index != block {
        return false;
    }
    let style = |index: Option<usize>| index.and_then(|index| fragments[index].style.as_ref());
    if let (Some(previous), Some(current)) =
        (style(previous.style_fragment), style(region.style_fragment))
    {
        if previous.differs_from(current) {
            return false;
        }
    }
    if previous.line_height.max(region.line_height)
        > 1.45 * previous.line_height.min(region.line_height)
    {
        return false;
    }
    let previous_bounds = previous.bounds.reading_box(region.rotation);
    let bounds = region.bounds.reading_box(region.rotation);
    let (horizontal, vertical) = previous_bounds.overlap(bounds);
    horizontal > 0.0
        && (vertical > 0.001
            || (block.is_some()
                && horizontal >= 0.4 * previous_bounds.width.min(bounds.width)
                && -vertical <= 0.9 * previous.line_height.min(region.line_height)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment(x: f64, y: f64, block: usize, position: usize) -> SourceFragment {
        SourceFragment {
            bounds: Some(Bounds {
                x,
                y,
                width: 0.3,
                height: 0.02,
            }),
            ink_boxes: Vec::new(),
            line_height: 0.02,
            rotation: Some(0),
            start_utf16: position,
            block_index: Some(block),
            pending: false,
            style: Some(SourceStyle::test_style([1.0; 3], 0.015)),
        }
    }

    #[test]
    fn plans_column_and_color_boundaries_and_preserves_pending_source() {
        let mut colored = fragment(0.1, 0.26, 1, 30);
        colored.style = Some(SourceStyle::test_style([0.6, 1.0, 0.7], 0.015));
        let mut pending = fragment(0.55, 0.205, 2, 50);
        pending.pending = true;
        let regions = plan(&[
            fragment(0.1, 0.20, 1, 10),
            fragment(0.1, 0.23, 1, 20),
            colored,
            fragment(0.55, 0.20, 2, 40),
            pending,
        ])
        .unwrap();
        assert_eq!(
            regions
                .regions
                .iter()
                .map(|region| region.fragments.as_slice())
                .collect::<Vec<_>>(),
            [&[0, 1][..], &[2][..]]
        );
        assert_eq!(regions.pending, [4]);
        assert_eq!(regions.overflow, [3]);
    }

    #[test]
    fn overlapping_source_blocks_are_not_painted_over_each_other() {
        let page = plan(&[
            fragment(0.1, 0.2, 1, 10),
            fragment(0.15, 0.207, 2, 20),
            fragment(0.6, 0.2, 3, 30),
        ])
        .unwrap();
        assert_eq!(page.overflow, [0, 1]);
        assert_eq!(page.regions[0].fragments, [2]);
    }

    #[test]
    fn preserves_clear_sentences_in_blocks_that_overlap_another_block() {
        let fragments = [
            fragment(0.1, 0.20, 1, 10),
            fragment(0.1, 0.23, 1, 20),
            fragment(0.3, 0.24, 2, 30),
            fragment(0.3, 0.27, 2, 40),
        ];
        let page = plan(&fragments).unwrap();
        assert_eq!(page.overflow, [1, 2]);
        assert_eq!(page.regions.iter().map(|region| region.fragments.as_slice()).collect::<Vec<_>>(),
            [&[0][..], &[3][..]]);

        let mut fragments = fragments;
        fragments[2].pending = true;
        let page = plan(&fragments).unwrap();
        assert_eq!(page.pending, [2]);
        assert_eq!(page.overflow, [1]);
        assert_eq!(page.regions.iter().map(|region| region.fragments[0]).collect::<Vec<_>>(), [3, 0]);
    }

    #[test]
    fn a_combined_translation_box_does_not_cover_an_adjacent_caption() {
        let left = || fragment(0.10, 0.20, 1, 10);
        let next = || fragment(0.25, 0.234, 1, 20);
        let caption = || fragment(0.45, 0.20, 2, 30);
        let unoccupied = plan(&[left(), next()]).unwrap();
        assert_eq!(unoccupied.regions[0].fragments, [0, 1]);

        for order in [[left(), next(), caption()], [caption(), left(), next()]] {
            let page = plan(&order).unwrap();
            assert!(page.overflow.is_empty());
            assert_eq!(page.regions.len(), 3);
            assert!(page.regions.iter().all(|region| region.fragments.len() == 1));
        }

        let mut pending = caption();
        pending.pending = true;
        pending.block_index = Some(1);
        let page = plan(&[left(), next(), pending]).unwrap();
        assert!(page.overflow.is_empty());
        assert_eq!(page.pending, [2]);
        assert_eq!(page.regions.len(), 2);
    }

    #[test]
    fn groups_adjacent_rotated_fragments_inside_only_their_document_block() {
        let make = |x, block, index| {
            let mut result = fragment(x, 0.2, block, index);
            result.bounds = Some(Bounds { x, y: 0.2, width: 0.02, height: 0.3 });
            result.rotation = Some(90);
            result
        };
        let page = plan(&[make(0.8, 1, 0), make(0.773, 1, 7), make(0.746, 2, 14)]).unwrap();
        assert_eq!(page.regions.len(), 2);
        assert_eq!(page.regions[0].fragments, [0, 1]);
        assert_eq!(page.regions[1].fragments, [2]);
        assert_eq!(page.regions[0].rotation, 90);
        assert!((page.regions[0].bounds.width - 0.047).abs() < 0.00001);
    }

    #[test]
    fn aligns_translated_lines_to_stable_source_margins() {
        let bounds = |x, y, width| Bounds { x, y, width, height: 0.04 };
        let first = bounds(0.2, 0.1, 0.5);
        assert_eq!(infer_alignment([first, bounds(0.4, 0.17, 0.3)].into_iter(), 0.04), Alignment::Right);
        assert_eq!(infer_alignment([first, bounds(0.3, 0.17, 0.3)].into_iter(), 0.04), Alignment::Center);
        assert_eq!(infer_alignment([first, bounds(0.2, 0.17, 0.3)].into_iter(), 0.04), Alignment::Left);
        assert_eq!(infer_alignment([first].into_iter(), 0.04), Alignment::Left);
    }
}
