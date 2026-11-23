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
}

struct SourceFragment {
    bounds: Option<Bounds>,
    line_height: f64,
    start_utf16: usize,
    block_index: Option<usize>,
    pending: bool,
    style: Option<SourceStyle>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    bounds: Bounds,
    line_height: f64,
    preferred_font_height: f64,
    limit_font_by_width: bool,
    fragments: Vec<usize>,
    style_fragment: Option<usize>,
    masks: Vec<masks::BackgroundMask>,
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
        let mut region = Region {
            bounds,
            line_height: fragment.line_height,
            preferred_font_height: 0.0,
            limit_font_by_width: false,
            fragments: vec![index],
            style_fragment: fragment.style.as_ref().map(|_| index),
            masks: Vec::new(),
        };
        while let Some(position) = page
            .regions
            .iter()
            .rposition(|previous| fits(previous, &region, fragment.block_index, fragments))
        {
            let previous = page.regions.remove(position);
            region.bounds = region.bounds.union(previous.bounds);
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
        let font = region
            .style_fragment
            .and_then(|index| fragments[index].style.as_ref()?.font_height)
            .filter(|height| *height > 0.0);
        region.preferred_font_height = font.map_or(region.line_height * 0.82, |height| {
            height.min(region.line_height * 0.90)
        });
        region.limit_font_by_width = font.is_none();
        page.regions.push(region);
    }
    page.regions.retain_mut(|region| {
        let covers_pending = page
            .pending
            .iter()
            .filter_map(|&index| fragments[index].bounds)
            .any(|bounds| {
                let (width, height) = region.bounds.overlap(bounds);
                width > 0.001 && height > 0.001
            });
        if covers_pending {
            page.overflow.append(&mut region.fragments);
        }
        !covers_pending
    });
    remove_overlapping_regions(&mut page);
    page.overflow
        .sort_unstable_by_key(|&index| (fragments[index].start_utf16, index));
    Ok(page)
}

fn remove_overlapping_regions(page: &mut PageLayout) {
    let mut colliding = vec![false; page.regions.len()];
    for (index, region) in page.regions.iter().enumerate() {
        for (other, candidate) in page.regions.iter().enumerate().skip(index + 1) {
            let (width, height) = region.bounds.overlap(candidate.bounds);
            if width > 0.001 && height > 0.001 {
                colliding[index] = true;
                colliding[other] = true;
            }
        }
    }
    let mut colliding = colliding.into_iter();
    page.regions.retain_mut(|region| {
        let conflict = colliding.next().unwrap_or(false);
        if conflict {
            page.overflow.append(&mut region.fragments);
        }
        !conflict
    });
}

fn fits(
    previous: &Region,
    region: &Region,
    block: Option<usize>,
    fragments: &[SourceFragment],
) -> bool {
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
    let (horizontal, vertical) = previous.bounds.overlap(region.bounds);
    horizontal > 0.0
        && (vertical > 0.001
            || (block.is_some()
                && horizontal >= 0.4 * previous.bounds.width.min(region.bounds.width)
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
            line_height: 0.02,
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
}
