use crate::ocr::OcrLine;

/// Join an isolated ornamental initial with the lowercase text aligned to its top right.
pub(super) fn join(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    let mut removed = vec![false; lines.len()];
    let mut output = None;
    for (index, cap) in lines.iter().enumerate() {
        let mut letters = cap.text.chars();
        if !letters.next().is_some_and(char::is_uppercase) || letters.next().is_some() {
            continue;
        }
        let current = output.as_deref().unwrap_or(lines);
        let nearest = current
            .iter()
            .enumerate()
            .filter(|&(index, _)| !removed[index])
            .filter_map(|(index, line)| distance(cap, line).map(|score| (index, score)))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let Some((target, _)) = nearest else {
            continue;
        };
        let result = output.get_or_insert_with(|| lines.to_vec());
        prefix(&mut result[target], cap);
        removed[index] = true;
    }
    output.map(|lines| {
        lines
            .into_iter()
            .zip(removed)
            .filter_map(|(line, removed)| (!removed).then_some(line))
            .collect()
    })
}

fn distance(cap: &OcrLine, body: &OcrLine) -> Option<f64> {
    let start = body.text.trim_start_matches(['’', '\'', 'ʼ']);
    let height = body.height;
    if !start.chars().next().is_some_and(char::is_lowercase)
        || body.text.chars().take(3).count() < 3
        || height <= 0.0
        || !(1.35 * height..=5.0 * height).contains(&cap.height)
    {
        return None;
    }
    let gap = body.x - (cap.x + cap.width);
    let rise = body.y - cap.y;
    ((-0.15 * height..=0.55 * height).contains(&gap)
        && (-0.20 * height..=0.85 * height).contains(&rise))
    .then_some(gap.abs() + rise.abs())
}

fn prefix(body: &mut OcrLine, cap: &OcrLine) {
    let units = cap.text.encode_utf16().count();
    for word in &mut body.words {
        if word.raw_start_utf16 == Some(0) {
            word.text.insert_str(0, &cap.text);
            let left = word.x.min(cap.x);
            let top = word.y.min(cap.y);
            word.width = (word.x + word.width).max(cap.x + cap.width) - left;
            word.height = (word.y + word.height).max(cap.y + cap.height) - top;
            word.x = left;
            word.y = top;
        } else if let Some(offset) = &mut word.raw_start_utf16 {
            *offset += units;
        }
        word.source_span = None;
    }
    let left = body.x.min(cap.x);
    let top = body.y.min(cap.y);
    body.width = (body.x + body.width).max(cap.x + cap.width) - left;
    body.height = (body.y + body.height).max(cap.y + cap.height) - top;
    body.x = left;
    body.y = top;
    body.block_start |= cap.block_start;
    body.source_span = None;
    body.text.insert_str(0, &cap.text);
}
