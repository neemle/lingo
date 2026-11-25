use super::{OcrLine, OcrWord};
use crate::Result;
use std::collections::BTreeMap;

/// Convert one Tesseract page into the normalized, upright OCR geometry used by the reader.
pub fn parse_tesseract_tsv(tsv: &str, width: u32, height: u32) -> Result<Vec<OcrLine>> {
    parse_lines(tsv, width, height, false)
}

/// Recover lines that read left to right in the rotated image and map their
/// bounds back to the original page, keeping the original reading direction.
pub fn parse_tesseract_quarter_turn(
    tsv: &str, width: u32, height: u32, angle: u16,
) -> Result<Vec<OcrLine>> {
    if !matches!(angle, 90 | 180 | 270) {
        return Err("Некоректний кут повороту OCR.".into());
    }
    let lines = parse_lines(tsv, width, height, true)?;
    let to_original = |x, y, w, h| super::orientation::reading_box((x, y, w, h), angle);
    Ok(lines.into_iter().filter_map(|mut line| {
        let is_initial = crate::ocr_layout::is_initial(&line.text);
        let accepted = if is_initial {
            line.confidence.is_some_and(|score| score >= 0.90)
        } else {
            line.confidence.is_some_and(|score| score >= 0.85)
                && line.width * f64::from(width) > line.height * f64::from(height)
                && line.words.first()?.x <= line.words.last()?.x
        };
        if !accepted {
            return None;
        }
        (line.x, line.y, line.width, line.height) = to_original(line.x, line.y, line.width, line.height);
        for word in &mut line.words {
            (word.x, word.y, word.width, word.height) = to_original(word.x, word.y, word.width, word.height);
        }
        line.rotation = Some(360 - angle);
        Some(line)
    }).collect())
}

fn parse_lines(tsv: &str, width: u32, height: u32, sparse: bool) -> Result<Vec<OcrLine>> {
    if tsv.len() > 16 * 1024 * 1024 || !(1..=4096).contains(&width) || !(1..=4096).contains(&height)
    {
        return Err("Результат OCR або розмір зображення перевищує допустимі межі.".into());
    }
    let mut rows = tsv.lines();
    if rows.next() != Some("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext") {
        return Err("Tesseract повернув некоректний формат TSV.".into());
    }
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    let mut word_count = 0;
    for row in rows.filter(|row| row.starts_with("5\t")) {
        let mut fields = row.splitn(12, '\t');
        let fields: [_; 12] = std::array::from_fn(|_| fields.next().unwrap_or(""));
        if fields[1] != "1" {
            continue;
        }
        let [Ok(block), Ok(paragraph), Ok(line), Ok(x), Ok(y), Ok(w), Ok(h)] =
            [2, 3, 4, 6, 7, 8, 9].map(|index| fields[index].parse::<i32>())
        else {
            continue;
        };
        let normalize =
            |coordinate: f64, extent: u32| (coordinate / f64::from(extent)).clamp(0.0, 1.0);
        let left = normalize(f64::from(x), width);
        let top = normalize(f64::from(y), height);
        let right = normalize(f64::from(x) + f64::from(w), width);
        let bottom = normalize(f64::from(y) + f64::from(h), height);
        if right <= left || bottom <= top || fields[11].contains('\0') || fields[11].len() > 8192 {
            continue;
        }
        let text = fields[11].split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        let confidence = fields[10]
            .parse::<f64>()
            .ok()
            .filter(|score| (0.0..=100.0).contains(score))
            .map(|score| score / 100.0);
        groups.entry((block, paragraph, line)).or_default().push((
            OcrWord {
                text,
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
                raw_start_utf16: None,
                source_span: None,
            },
            confidence,
        ));
        word_count += 1;
        if word_count > 65_536 || groups.len() > 8192 {
            return Err("Результат OCR перевищує допустимий розмір.".into());
        }
    }

    let mut lines = groups
        .into_iter()
        .map(|((block, paragraph, _), group)| {
            // A checkbox on a coloured panel is sometimes read as half a
            // bracket. Leave its pixels to the PDF renderer and anchor the
            // recovered text at the first actual word.
            let decorative_prefix = sparse && group.len() >= 3
                && group[0].1.is_some_and(|score| score < 0.75)
                && matches!(group[0].0.text.as_str(), "[" | "]" | "[1" | "1]" | "[]" | "□" | "☐")
                && group[1].0.text.starts_with(char::is_alphabetic);
            let mut line = OcrLine {
                rotation: None,
                page: 0,
                text: String::new(),
                x: 1.0,
                y: 1.0,
                width: 0.0,
                height: 0.0,
                confidence: None,
                correction: None,
                block_start: false,
                words: Vec::with_capacity(group.len()),
                source_span: None,
            };
            let (mut right, mut bottom, mut offset, mut score, mut scored) =
                (0.0_f64, 0.0_f64, 0, 0.0, 0);
            for (mut word, confidence) in group.into_iter().skip(usize::from(decorative_prefix)) {
                if !line.text.is_empty() {
                    line.text.push(' ');
                    offset += 1;
                }
                word.raw_start_utf16 = Some(offset);
                offset += word.text.encode_utf16().count();
                line.text.push_str(&word.text);
                if line.text.len() > 8192 {
                    return Err("Рядок OCR завеликий.".into());
                }
                line.x = line.x.min(word.x);
                line.y = line.y.min(word.y);
                right = right.max(word.x + word.width);
                bottom = bottom.max(word.y + word.height);
                if let Some(confidence) = confidence {
                    score += confidence;
                    scored += 1;
                }
                line.words.push(word);
            }
            line.width = right - line.x;
            line.height = bottom - line.y;
            line.confidence = (scored > 0).then(|| score / f64::from(scored));
            Ok(((block, paragraph), line))
        })
        .collect::<Result<Vec<_>>>()?;
    for index in 1..lines.len() {
        let (previous_group, previous) = &lines[index - 1];
        let (group, current) = &lines[index];
        if previous_group != group && starts_paragraph(previous, current) {
            lines[index].1.block_start = true;
        }
    }
    Ok(lines.into_iter().map(|(_, line)| line).collect())
}

fn starts_paragraph(previous: &OcrLine, next: &OcrLine) -> bool {
    let height = previous.height.min(next.height);
    let gap = next.y - previous.y - previous.height;
    if gap < -0.2 * height || next.y <= previous.y {
        return false;
    }
    let before = previous.text.trim_end_matches(['»', '”', '"', '\'', ')', ' ']);
    let after = next.text.trim_start_matches(['«', '“', '"', ' ']);
    if let Some(stem) = before.strip_suffix('.') {
        let last = stem.rsplit(|ch: char| !ch.is_alphabetic()).next().unwrap_or("");
        if crate::pdf_sentences::is_abbreviated_title(last) {
            return false;
        }
    }
    if before.ends_with(['-', '‐', '‑', '\u{00ad}']) {
        return false;
    }
    if crate::document_layout::keep_pdf_sentence_break(&previous.text, &next.text) {
        return true;
    }
    let first_word = &next.words[0];
    let glyph = first_word.width / first_word.text.chars().count() as f64;
    let indent = next.x - previous.x;
    after.starts_with(char::is_uppercase) &&
        (gap >= 0.25 * height && before.ends_with(['.', '?', '!', '…']) ||
         gap >= 0.0 && (2.5 * glyph..=0.08).contains(&indent))
}

/// Use sparse OCR to fill holes without replacing primary text or its reading order.
pub fn recover_tesseract_lines(
    mut primary: Vec<OcrLine>, sparse_tsv: &str, width: u32, height: u32,
) -> Vec<OcrLine> {
    if primary.is_empty() {
        return primary;
    }
    let Ok(sparse) = parse_lines(sparse_tsv, width, height, true) else {
        return primary;
    };
    // Normalized horizontal and vertical coordinates use different scales on portrait pages.
    crate::ocr_layout::recover_sparse_initials(&mut primary, &sparse, f64::from(width) / f64::from(height));
    merge_missing_lines(primary, sparse)
}

pub(crate) fn merge_missing_lines(primary: Vec<OcrLine>, sparse: Vec<OcrLine>) -> Vec<OcrLine> {
    let horizontal_overlap = |a: &OcrLine, b: &OcrLine| {
        (a.x + a.width).min(b.x + b.width) - a.x.max(b.x)
    };
    let center = |line: &OcrLine| line.y + line.height / 2.0;
    let same_ink = |a: &OcrLine, b: &OcrLine| {
        let vertical = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
        vertical >= 0.35 * a.height.min(b.height)
            && horizontal_overlap(a, b) >= 0.20 * a.width.min(b.width)
    };
    let mut additions = BTreeMap::<usize, Vec<OcrLine>>::new();
    let mut line_count = primary.len();
    let mut word_count = primary.iter().map(|line| line.words.len()).sum::<usize>();
    for mut line in sparse.into_iter().filter(|line| {
        line.confidence.is_some_and(|score| score >= 0.88)
            && line.words.iter().filter(|word| word.text.chars().any(char::is_alphabetic)).take(2).count() >= 2
            && line.text.chars().filter(|ch| ch.is_alphabetic()).take(8).count() >= 8
    }).take(256) {
        if line_count >= 8192 || word_count + line.words.len() > 65_536 {
            break;
        }
        if primary.iter().any(|row| same_ink(row, &line))
            || additions.values().flatten().any(|row| same_ink(row, &line))
        {
            continue;
        }
        let Some((index, anchor)) = primary.iter().enumerate()
            .filter(|(_, row)| horizontal_overlap(row, &line) >= 0.35 * row.width.min(line.width))
            .min_by(|(_, a), (_, b)| (center(a) - center(&line)).abs().total_cmp(&(center(b) - center(&line)).abs()))
        else {
            continue;
        };
        let insertion = index + usize::from(center(anchor) < center(&line));
        word_count += line.words.len();
        line_count += 1;
        // Sparse OCR treats each detection as its own block. Rebuild
        // paragraph boundaries using the merged neighbours instead.
        line.block_start = false;
        additions.entry(insertion).or_default().push(line);
    }
    if additions.is_empty() {
        return primary;
    }
    let mut merged = Vec::with_capacity(line_count);
    for (index, primary) in primary.into_iter().map(Some).chain([None]).enumerate() {
        if let Some(mut rows) = additions.remove(&index) {
            rows.sort_by(|a, b| a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x)));
            for mut row in rows {
                row.block_start = merged.last().is_some_and(|previous| starts_paragraph(previous, &row));
                merged.push(row);
            }
        }
        merged.extend(primary);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tesseract_tsv_keeps_line_geometry_and_utf16_word_positions() {
        let tsv = concat!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            "5\t1\t2\t1\t1\t1\t60\t50\t25\t10\t70\tAutre\n",
            "5\t1\t1\t1\t1\t1\t10\t10\t20\t10\t90\t𐐷\n",
            "5\t1\t1\t1\t1\t2\t35\t10\t15\t10\t80\trègle\n",
        );
        let lines = parse_tesseract_tsv(tsv, 100, 100).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "𐐷 règle");
        assert_eq!(lines[0].words[1].raw_start_utf16, Some(3));
        assert!((lines[0].width - 0.4).abs() < 1e-9);
        assert!((lines[0].confidence.unwrap() - 0.85).abs() < 1e-9);
        assert_eq!(lines[1].text, "Autre");
    }

    #[test]
    fn restores_ocr_paragraph_breaks_without_breaking_a_wrapped_title() {
        let tsv = concat!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            "5\t1\t1\t1\t1\t1\t10\t10\t40\t10\t90\tLe marin arrive.\n",
            "5\t1\t1\t2\t1\t1\t10\t26\t50\t10\t90\tIl aperçoit M.\n",
            "5\t1\t1\t3\t1\t1\t10\t42\t55\t10\t90\tMorrel sur le quai.\n",
            "5\t1\t1\t3\t2\t1\t10\t58\t70\t10\t90\tIl appelle le capitaine.\n",
            "5\t1\t2\t1\t1\t1\t10\t74\t45\t10\t90\t— Bonjour !\n",
        );
        let result = parse_tesseract_tsv(tsv, 100, 100).unwrap();
        assert_eq!(result.iter().map(|line| line.block_start).collect::<Vec<_>>(),
            [false, true, false, false, true]);
        let text = crate::ocr::recognized_lines(&result, 1).unwrap();
        assert_eq!(text, "Le marin arrive.\n\nIl aperçoit M.\nMorrel sur le quai.\nIl appelle le capitaine.\n\n— Bonjour !");
    }

    #[test]
    fn quarter_turn_retains_only_geometrically_matched_drop_initials() {
        let tsv = concat!(
            "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n",
            "5\t1\t1\t1\t1\t1\t80\t295\t35\t46\t96\tL\n",
            "5\t1\t2\t1\t1\t1\t125\t300\t20\t20\t97\te\n",
            "5\t1\t2\t1\t1\t2\t150\t300\t110\t20\t97\tbateau\n",
            "5\t1\t3\t1\t1\t1\t600\t100\t35\t46\t96\tX\n",
        );
        for angle in [90, 180, 270] {
            let size = if angle == 180 { [1000, 1500] } else { [1500, 1000] };
            let lines = parse_tesseract_quarter_turn(tsv, 1000, 1500, angle).unwrap();
            let merged = crate::ocr::merge_rotated_lines(Vec::new(), lines, Some(size)).unwrap();
            assert_eq!(merged.len(), 1, "{angle}°");
            assert_eq!(merged[0].text, "Le bateau");
            assert_eq!(merged[0].rotation, Some(360 - angle));
            assert_eq!(merged[0].words[0].text, "Le");
            assert_eq!(merged[0].words[1].raw_start_utf16, Some(3));
            let first = &merged[0].words[0];
            let (x, y, w, h) = crate::ocr::reading_box(
                (first.x, first.y, first.width, first.height), 360 - angle,
            );
            assert!((x - 0.08).abs() < 1e-9 && (y - 295.0 / 1500.0).abs() < 1e-9);
            assert!((w - 0.065).abs() < 1e-9 && (h - 46.0 / 1500.0).abs() < 1e-9);
        }
    }

    #[test]
    fn sparse_ocr_adds_supported_missing_rows_without_duplicating_primary_text() {
        const HEADER: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
        let primary = format!("{HEADER}{}", concat!(
            "5\t1\t1\t1\t1\t1\t10\t10\t75\t5\t96\tFirst paragraph.\n",
            "5\t1\t1\t1\t2\t1\t10\t40\t75\t5\t96\tThird paragraph.\n",
        ));
        let sparse = format!("{HEADER}{}", concat!(
            "5\t1\t1\t1\t1\t1\t10\t11\t25\t5\t99\tFirst\n",
            "5\t1\t1\t1\t1\t2\t45\t11\t30\t5\t99\tparagraph.\n",
            "5\t1\t2\t1\t1\t1\t3\t22\t5\t5\t10\t[1\n",
            "5\t1\t2\t1\t1\t2\t10\t22\t24\t5\t94\tSecond\n",
            "5\t1\t2\t1\t1\t3\t43\t22\t35\t5\t94\tparagraph.\n",
            "5\t1\t3\t1\t1\t1\t10\t32\t25\t5\t9\tdotted\n",
            "5\t1\t3\t1\t1\t2\t43\t32\t25\t5\t9\tpattern\n",
        ));
        let primary = parse_tesseract_tsv(&primary, 100, 100).unwrap();
        let merged = recover_tesseract_lines(primary, &sparse, 100, 100);
        assert_eq!(merged.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(),
            ["First paragraph.", "Second paragraph.", "Third paragraph."]);
        assert_eq!(merged[1].words[1].raw_start_utf16, Some(7));
        assert_eq!(merged[1].x, 0.1);
        assert!(merged[1].block_start);
    }

    #[test]
    fn sparse_ocr_restores_an_initial_missing_from_the_primary_pass() {
        const HEADER: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
        let body = concat!(
            "5\t1\t1\t1\t1\t1\t25\t20\t6\t10\t97\te\n",
            "5\t1\t1\t1\t1\t2\t35\t20\t35\t10\t97\tmarin\n",
        );
        let initial = "5\t1\t2\t1\t1\t1\t10\t17\t13\t22\t96\tL\n";
        let parsed = parse_tesseract_tsv(&format!("{HEADER}{body}"), 100, 100).unwrap();
        let sparse = format!("{HEADER}{initial}");
        let mut lines = recover_tesseract_lines(parsed.clone(), &sparse, 100, 100);
        let text = crate::ocr::recognized_lines(&lines, 1).unwrap();
        assert_eq!(text, "Le marin");
        crate::ocr::refresh_line_provenance(&text, &mut lines, None);
        assert_eq!(lines[0].words[0].x, 0.1);
        assert_eq!(lines[0].words[0].source_span.unwrap().end_utf16, 2);
        assert_eq!(lines[0].words[1].source_span.unwrap().start_utf16, 3);

        let weak = sparse.replace("\t96\tL", "\t30\tL");
        assert_eq!(recover_tesseract_lines(parsed, &weak, 100, 100)[0].text, "e marin");
        let original = parse_tesseract_tsv(&format!("{HEADER}{body}{initial}"), 100, 100).unwrap();
        let mut already_present = recover_tesseract_lines(original, &sparse, 100, 100);
        crate::ocr_layout::reconstruct_blocks(&mut already_present);
        assert_eq!(crate::ocr::recognized_lines(&already_present, 1).unwrap(), "Le marin");

        let primary = format!("{HEADER}{}", concat!(
            "5\t1\t1\t1\t1\t1\t10\t17\t21\t23\t55\tL_:\n",
            "5\t1\t1\t1\t1\t2\t35\t20\t9\t10\t97\t15\n",
            "5\t1\t1\t1\t1\t3\t47\t19\t25\t11\t97\tfévrier\n",
        ));
        let sparse = format!("{HEADER}{initial}{}", concat!(
            "5\t1\t3\t1\t1\t1\t25\t20\t6\t10\t97\te\n",
            "5\t1\t3\t1\t1\t2\t35\t20\t9\t10\t97\t15\n",
            "5\t1\t3\t1\t1\t3\t47\t19\t25\t11\t97\tfévrier\n",
        ));
        let conflicting = sparse.replace("février", "janvier");
        for damaged in ["L_:", "[L_:", "©"] {
            let input = parse_tesseract_tsv(&primary.replace("L_:", damaged), 100, 100).unwrap();
            let mut merged = recover_tesseract_lines(input.clone(), &sparse, 100, 100);
            assert_eq!(merged[0].text, "Le 15 février");
            assert_eq!(merged[0].words[0].text, "Le");
            assert_eq!(merged[0].words[0].x, 0.1);
            assert_eq!(merged[0].words[1].raw_start_utf16, Some(3));
            crate::ocr::refresh_line_provenance("Le 15 février", &mut merged, None);
            assert_eq!(merged[0].words[0].source_span.unwrap().end_utf16, 2);
            assert_eq!(recover_tesseract_lines(input, &conflicting, 100, 100)[0].text,
                format!("{damaged} 15 février"));
        }
        let bullet = parse_tesseract_tsv(&primary.replace("L_:", "•"), 100, 100).unwrap();
        assert_eq!(recover_tesseract_lines(bullet, &sparse, 100, 100)[0].text, "• 15 février");
    }
}
