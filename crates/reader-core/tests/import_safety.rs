use reader_core::Store;
use std::io::{Cursor, Write};

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
fn truncated_docx_is_rejected_without_persisting_a_document() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("reader.db")).unwrap();
    let mut bytes = docx(b"<w:document/>", zip::CompressionMethod::Deflated);
    bytes.truncate(bytes.len() / 2);
    assert!(store.import("broken.docx", &bytes, "en").is_err());
    assert!(store.snapshot().unwrap().documents.is_empty());
}
