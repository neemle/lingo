use reader_core::{file_originals, intake::IntakeInput, Store};
use std::{fs, io::{Cursor, Write}};

fn pdf_with_catalog_nesting(depth: usize) -> Vec<u8> {
    let contents = "BT /F1 18 Tf 72 700 Td (Safe reader.) Tj ET";
    let objects = [
        format!("<< /Type /Catalog /Pages 2 0 R /X {}0{} >>", "[".repeat(depth), "]".repeat(depth)),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!("<< /Length {} >>\nstream\n{contents}\nendstream", contents.len()),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        write!(bytes, "{} 0 obj\n{object}\nendobj\n", index + 1).unwrap();
    }
    let xref = bytes.len();
    write!(bytes, "xref\n0 6\n0000000000 65535 f \n").unwrap();
    for offset in offsets {
        writeln!(bytes, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        bytes,
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    )
    .unwrap();
    bytes
}

#[test]
fn excessive_pdf_nesting_is_rejected_and_original_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("reader.db")).unwrap();
    let valid = pdf_with_catalog_nesting(4);
    let document = store.import("safe.pdf", &valid, "en").unwrap();
    assert!(document.text.contains("Safe reader."));
    assert_eq!(store.original(&document.id).unwrap().bytes, valid);
    assert!(store
        .import("nested.pdf", &pdf_with_catalog_nesting(1000), "en")
        .is_err());
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.documents.len(), 1);
    assert_eq!(snapshot.documents[0].id, document.id);
    assert_eq!(store.original(&document.id).unwrap().bytes, valid);
}

#[test]
fn staged_pdf_matches_existing_original_by_content_not_size() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("reader.db")).unwrap();
    let path = dir.path().join("book.pdf");
    let original = pdf_with_catalog_nesting(4);
    fs::write(&path, &original).unwrap();
    let prepared = file_originals::prepare_pdf_import(
        store.pdf_original_root(), "book.pdf", &path, "en", vec!["Safe reader.".into()], vec![],
    ).unwrap();
    let saved = store.import_pdf_file(prepared).unwrap();
    let staged = |store: &mut Store| {
        let item = store.stage_intake(IntakeInput::PdfFile {
            name: "book.pdf".into(),
            path: path.clone(),
        }).unwrap();
        serde_json::to_value(item).unwrap()
    };
    let duplicate = staged(&mut store);
    assert_eq!(duplicate["nextAction"], "complete");
    assert_eq!(duplicate["documentId"], saved.id);

    let mut different = original;
    *different.last_mut().unwrap() = b'\r';
    fs::write(&path, different).unwrap();
    let same_size = staged(&mut store);
    assert_eq!(same_size["nextAction"], "readPdfPages");
    assert!(same_size["documentId"].is_null());
}

fn docx(xml: &[u8], method: zip::CompressionMethod) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
    writer.write_all(xml).unwrap();
    writer.finish().unwrap().into_inner()
}

#[test]
fn stored_and_deflated_docx_imports_preserve_text_and_originals() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("reader.db")).unwrap();
    for method in [
        zip::CompressionMethod::Stored,
        zip::CompressionMethod::Deflated,
    ] {
        let bytes = docx(br#"<w:document xmlns:w="test"><w:p><w:r><w:t>Tom &amp; Jerry &#x1F600;.</w:t></w:r></w:p><w:p><w:r><w:t>Next.</w:t></w:r></w:p></w:document>"#, method);
        let document = store.import("story.docx", &bytes, "en").unwrap();
        assert_eq!(document.text, "Tom & Jerry 😀.\nNext.\n");
        assert_eq!(document.sentences.len(), 2);
        assert_eq!(store.original(&document.id).unwrap().bytes, bytes);
    }
}

#[test]
fn docx_run_controls_preserve_visible_text_and_ignore_tab_stops() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("reader.db")).unwrap();
    let bytes = docx(br#"
        <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
          <w:body>
            <w:p>
              <w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr>
              <w:r><w:t>Jean</w:t><w:noBreakHyphen/><w:t>Luc.</w:t><w:cr></w:cr>
                <w:t>Un</w:t><w:tab/><w:t>Deux</w:t><w:ptab w:alignment="center"
                  w:relativeTo="margin" w:leader="none"/><w:t>Trois.</w:t><w:br/>
                <w:t>Ex</w:t><w:softHyphen/><w:t>emple.</w:t>
              </w:r>
            </w:p>
          </w:body>
        </w:document>"#, zip::CompressionMethod::Deflated);
    let document = store.import("controls.docx", &bytes, "fr").unwrap();
    assert_eq!(document.text, "Jean‑Luc.\nUn\tDeux\tTrois.\nExemple.\n");
    assert_eq!(document.sentences.len(), 3);
    assert_eq!(store.original(&document.id).unwrap().bytes, bytes);
}

#[test]
fn docx_import_uses_current_runs_and_paragraph_properties() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("reader.db")).unwrap();
    let bytes = docx(br#"
        <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
          <w:body>
            <w:p>
              <w:pPr><w:outlineLvl w:val="1"/>
                <w:pPrChange w:id="1" w:author="Editor">
                  <w:pPr><w:outlineLvl w:val="9"/></w:pPr>
                </w:pPrChange>
              </w:pPr>
              <w:r><w:t xml:space="preserve">Chapter </w:t></w:r>
              <w:del w:id="2" w:author="Editor"><w:r><w:tab/><w:br/><w:delText>One</w:delText></w:r></w:del>
              <w:ins w:id="3" w:author="Editor"><w:r><w:t>Two</w:t></w:r></w:ins>
            </w:p>
            <w:p>
              <w:moveFrom w:id="4" w:author="Editor"><w:r><w:cr/><w:delText>Read this.</w:delText></w:r></w:moveFrom>
              <w:r><w:t>Start here.</w:t></w:r>
            </w:p>
            <w:p><w:moveTo w:id="5" w:author="Editor"><w:r><w:t>Read this.</w:t></w:r></w:moveTo></w:p>
          </w:body>
        </w:document>"#, zip::CompressionMethod::Deflated);
    let document = store.import("tracked.docx", &bytes, "en").unwrap();
    assert_eq!(document.text, "Chapter Two\nStart here.\nRead this.\n");
    assert_eq!(document.blocks[0].heading_level, Some(2));
    assert_eq!(document.blocks.len(), 3);
    assert_eq!(store.original(&document.id).unwrap().bytes, bytes);
}

#[test]
fn truncated_docx_is_rejected_without_persisting_a_document() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("reader.db")).unwrap();
    let mut bytes = docx(b"<w:document/>", zip::CompressionMethod::Deflated);
    bytes.truncate(bytes.len() / 2);
    assert!(store.import("broken.docx", &bytes, "en").is_err());
    assert!(store.snapshot().unwrap().documents.is_empty());
}
