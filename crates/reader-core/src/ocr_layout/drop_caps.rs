use crate::ocr::{reading_box, OcrLine};

/// Retain a sparse-only initial when its geometry matches a line in the primary OCR.
pub(crate) fn recover_sparse_initials(primary: &mut [OcrLine], sparse: &[OcrLine], aspect: f64) {
    for cap in sparse.iter().filter(|line| {
        line.correction.is_none() && is_initial(&line.text)
            && line.confidence.is_some_and(|score| score >= 0.90)
    }).take(32) {
        if nearest_body(cap, sparse, aspect)
            .is_some_and(|index| repair_damaged_initial(primary, cap, &sparse[index]))
        {
            continue;
        }
        if let Some(target) = nearest_body(cap, primary, aspect) {
            if !primary.iter().any(|line| is_initial(&line.text) && distance(line, &primary[target], aspect).is_some()) {
                prefix(&mut primary[target], cap);
            }
        }
    }
}

fn nearest_body(cap: &OcrLine, lines: &[OcrLine], aspect: f64) -> Option<usize> {
    lines.iter().enumerate()
        .filter_map(|(index, line)| distance(cap, line, aspect).map(|score| (index, score)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

fn repair_damaged_initial(primary: &mut [OcrLine], cap: &OcrLine, sparse_body: &OcrLine) -> bool {
    let [tail, first_anchor, second_anchor, ..] = sparse_body.words.as_slice() else { return false };
    if tail.raw_start_utf16 != Some(0) || !sparse_body.text.starts_with(&tail.text) {
        return false;
    }
    let mut candidates = primary.iter_mut().filter(|line| {
        let [damaged, first, second, ..] = line.words.as_slice() else { return false };
        line.correction.is_none() && line.page == cap.page
            && line.rotation.unwrap_or(0) == cap.rotation.unwrap_or(0)
            && damaged.raw_start_utf16 == Some(0)
            && (is_initial_artifact(&damaged.text) || damaged.text.len() > cap.text.len()
                && damaged.text.chars().filter(|ch| ch.is_alphanumeric()).eq(cap.text.chars()))
            && (damaged.x - cap.x).abs() <= cap.width * 0.35
            && (damaged.y - cap.y).abs() <= cap.height * 0.35
            && [first, second].into_iter().zip([first_anchor, second_anchor]).all(|(a, b)| {
                a.text == b.text && (a.y - b.y).abs() <= b.height * 0.25
                    && (a.x - b.x).abs() <= b.width * 0.25
            })
    });
    let Some(line) = candidates.next() else { return false };
    if candidates.next().is_some() || !line.text.starts_with(&line.words[0].text) {
        return false;
    }
    let damaged = &line.words[0].text;
    if line.text.len() - damaged.len() + tail.text.len() + cap.text.len() > 8192 {
        return false;
    }
    let original_units = damaged.encode_utf16().count();
    let recovered_units = tail.text.encode_utf16().count();
    line.text.replace_range(..damaged.len(), &tail.text);
    line.words[0] = tail.clone();
    for word in &mut line.words[1..] {
        word.raw_start_utf16 = word.raw_start_utf16
            .and_then(|offset| offset.checked_sub(original_units))
            .map(|offset| offset + recovered_units);
    }
    prefix(line, cap);
    true
}

pub(crate) fn is_initial_artifact(text: &str) -> bool {
    matches!(text, "©" | "®" | "@" | "§")
}

pub(crate) fn is_initial(text: &str) -> bool {
    let mut letters = text.chars();
    letters.next().is_some_and(char::is_uppercase) && letters.next().is_none()
}

/// Join an isolated ornamental initial with the lowercase text aligned to its top right.
pub(super) fn join(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    let mut removed = vec![false; lines.len()];
    let mut output = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(joined) = join_inline(line) {
            output.get_or_insert_with(|| lines.to_vec())[index] = joined;
        }
    }
    for (index, cap) in lines.iter().enumerate() {
        if !is_initial(&cap.text) {
            continue;
        }
        let current = output.as_deref().unwrap_or(lines);
        let nearest = current
            .iter()
            .enumerate()
            .filter(|&(index, _)| !removed[index])
            .filter_map(|(index, line)| distance(cap, line, 1.0).map(|score| (index, score)))
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

/// Some engines report the initial and the rest of the word in the same OCR row.
fn join_inline(line: &OcrLine) -> Option<OcrLine> {
    let [cap, tail, ..] = line.words.as_slice() else { return None };
    let angle = line.rotation.unwrap_or(0);
    let (cap_x, cap_y, cap_width, cap_height) = reading_box((cap.x, cap.y, cap.width, cap.height), angle);
    let (tail_x, tail_y, _, tail_height) = reading_box((tail.x, tail.y, tail.width, tail.height), angle);
    if cap.raw_start_utf16 != Some(0) || !is_initial(&cap.text)
        || !tail.text.chars().next().is_some_and(char::is_lowercase)
        || !close_to_initial(cap_x + cap_width, cap_y, cap_height, tail_x, tail_y, tail_height)
    {
        return None;
    }
    let tail_start = tail.raw_start_utf16?;
    let tail_byte = crate::ocr::byte_at_utf16(&line.text, tail_start)?;
    let gap = line.text.get(cap.text.len()..tail_byte)?;
    if gap.is_empty() || !gap.chars().all(char::is_whitespace) || !line.text.starts_with(&cap.text)
        || !line.text[tail_byte..].starts_with(&tail.text)
    {
        return None;
    }
    let removed_units = gap.encode_utf16().count();

    let mut joined = line.clone();
    joined.text.replace_range(cap.text.len()..tail_byte, "");
    joined.source_span = None;
    joined.words.remove(1);
    let first = &mut joined.words[0];
    first.text.push_str(&tail.text);
    first.x = cap.x.min(tail.x);
    first.y = cap.y.min(tail.y);
    first.width = (cap.x + cap.width).max(tail.x + tail.width) - first.x;
    first.height = (cap.y + cap.height).max(tail.y + tail.height) - first.y;
    first.source_span = None;
    for word in &mut joined.words[1..] {
        word.raw_start_utf16 = word.raw_start_utf16.and_then(|offset| offset.checked_sub(removed_units));
        word.source_span = None;
    }
    Some(joined)
}

fn distance(cap: &OcrLine, body: &OcrLine, aspect: f64) -> Option<f64> {
    let start = body.text.trim_start_matches(['’', '\'', 'ʼ']);
    let angle = body.rotation.unwrap_or(0);
    if cap.rotation.unwrap_or(0) != angle || cap.page != body.page
        || cap.correction.is_some() || body.correction.is_some()
        || !start.chars().next().is_some_and(char::is_lowercase)
        || body.text.chars().take(3).count() < 3 || body.text.len() + cap.text.len() > 8192
    {
        return None;
    }
    let (cap_x, cap_y, cap_width, cap_height) = cap.reading_bounds();
    let (x, y, _, height) = body.words.first().filter(|word| word.raw_start_utf16 == Some(0))
        .map_or_else(|| body.reading_bounds(), |word| reading_box((word.x, word.y, word.width, word.height), angle));
    let aspect = if matches!(angle, 90 | 270) { 1.0 / aspect } else { aspect };
    let right = (cap_x + cap_width) * aspect;
    close_to_initial(right, cap_y, cap_height, x * aspect, y, height)
        .then_some((x * aspect - right).abs() + (y - cap_y).abs())
}

fn close_to_initial(right: f64, top: f64, cap_height: f64, x: f64, y: f64, height: f64) -> bool {
    height > 0.0 && (1.35 * height..=5.0 * height).contains(&cap_height)
        && (-0.15 * height..=0.55 * height).contains(&(x - right))
        && (-0.20 * height..=0.85 * height).contains(&(y - top))
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
