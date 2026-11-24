mod support;

use reader_ffi::{reader_abi_version, reader_close, reader_release};
use serde_json::json;
use std::sync::{Arc, Barrier};
use support::{assert_error, call, Response, StoreFixture};

#[test]
fn malformed_requests_are_structured_errors() {
    assert_eq!(reader_abi_version(), 1);
    for input in [
        b"".as_slice(),
        b"{",
        b"\xff",
        b"null",
        b"{}",
        br#"{"version":1,"operation":"import","name":"test.txt"}"#,
    ] {
        assert_error(&Response::call(0, input).json(), "invalid_request");
    }
    // Null with zero length is a valid empty buffer, rejected as malformed JSON.
    let empty = unsafe { Response::raw(0, std::ptr::null(), 0) };
    assert_error(&empty.json(), "invalid_request");
    // These invalid pointer/length pairs must be rejected before dereferencing.
    let null_nonempty = unsafe { Response::raw(0, std::ptr::null(), 1) };
    assert_error(&null_nonempty.json(), "invalid_argument");
    let oversized = unsafe { Response::raw(0, std::ptr::null(), u64::MAX) };
    assert_error(&oversized.json(), "invalid_argument");
}

#[test]
fn passage_source_is_available_before_any_model_translation() {
    let store = StoreFixture::new();
    let source = "— Bonjour, monsieur.\n— Vous partez demain ?";
    let document = call(store.handle, json!({
        "version": 1, "operation": "import", "name": "dialogue.txt",
        "language": "fr", "bytes": source.as_bytes(),
    }));
    assert_eq!(document["ok"], true, "{document}");
    let reply = call(store.handle, json!({
        "version": 1, "operation": "passage_source",
        "id": document["data"]["id"], "sentenceId": document["data"]["sentences"][0]["id"],
    }));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["data"], source);
    let bad = call(store.handle, json!({
        "version": 1, "operation": "passage_source", "id": document["data"]["id"], "sentenceId": "",
    }));
    assert_error(&bad, "invalid_argument");
}

#[test]
fn canceled_pdf_translation_does_not_cancel_a_later_page_request() {
    let store = StoreFixture::new();
    let imported = call(store.handle, json!({
        "version": 1, "operation": "import_scanned_pdf", "name": "scan.pdf", "language": "fr",
        "bytes": b"%PDF-1.4\n", "pages": 1,
        "lines": [{"page": 0, "text": "Le bateau arrive.", "x": 0.1, "y": 0.2, "width": 0.8, "height": 0.1}],
    }));
    assert_eq!(imported["ok"], true, "{imported}");
    let id = &imported["data"]["id"];
    let cancel = call(store.handle, json!({
        "version": 1, "operation": "cancel_pdf_page_translation", "requestId": 5,
    }));
    assert_eq!(cancel["ok"], true, "{cancel}");
    let request = |request_id, background| call(store.handle, json!({
        "version": 1, "operation": "pdf_page_translation", "id": id, "page": 0,
        "target": "uk", "cachedOnly": true, "requestId": request_id, "background": background,
    }));
    assert_error(&request(5, false), "canceled");
    assert_eq!(request(5, true)["ok"], true);
    let cancel_background = call(store.handle, json!({
        "version": 1, "operation": "cancel_background_translation", "requestId": 6,
    }));
    assert_eq!(cancel_background["ok"], true, "{cancel_background}");
    assert_error(&request(6, true), "canceled");
    let advance = call(store.handle, json!({
        "version": 1, "operation": "advance_background_translation", "id": id,
        "revision": imported["data"]["currentRevisionId"], "target": "uk", "requestId": 6,
    }));
    assert_error(&advance, "canceled");
    let current = request(6, false);
    assert_eq!(current["ok"], true, "{current}");
    assert_eq!(current["data"].as_array().unwrap().len(), 1);
}

#[test]
fn canceled_word_explanation_leaves_new_queries_available() {
    let store = StoreFixture::new();
    let imported = call(store.handle, json!({
        "version": 1, "operation": "import", "name": "phrase.txt", "language": "fr",
        "bytes": b"Le bateau arrive.",
    }));
    assert_eq!(imported["ok"], true, "{imported}");
    let id = &imported["data"]["id"];
    let sentence = &imported["data"]["sentences"][0]["id"];
    let result = call(store.handle, json!({
        "version": 1, "operation": "cancel_word_explanation", "requestId": 5,
    }));
    assert_eq!(result["ok"], true, "{result}");
    let explain = |request_id| call(store.handle, json!({
        "version": 1, "operation": "explain_word", "model": "qwen-2b",
        "document": id, "sentence": sentence, "word": "bateau", "cachedOnly": true, "requestId": request_id,
    }));
    assert_error(&explain(Some(5)), "canceled");
    let detached = call(store.handle, json!({
        "version": 1, "operation": "explain_detached_word", "model": "qwen-2b",
        "document": id, "page": 0, "startUtf16": 3, "word": "bateau", "cachedOnly": true, "requestId": 4,
    }));
    assert_error(&detached, "canceled");
    for request_id in [Some(6), None] {
        let fresh = explain(request_id);
        assert_eq!(fresh["ok"], true, "{fresh}");
    }
}

#[test]
fn canceling_sentence_review_leaves_word_explanations_and_later_reviews_available() {
    let store = StoreFixture::new();
    let imported = call(store.handle, json!({
        "version": 1, "operation": "import", "name": "phrase.txt", "language": "fr",
        "bytes": b"Le bateau arrive.",
    }));
    assert_eq!(imported["ok"], true, "{imported}");
    let id = &imported["data"]["id"];
    let sentence = &imported["data"]["sentences"][0]["id"];
    let canceled = call(store.handle, json!({
        "version": 1, "operation": "cancel_sentence_review", "requestId": 5,
    }));
    assert_eq!(canceled["ok"], true, "{canceled}");
    let review = |request_id| call(store.handle, json!({
        "version": 1, "operation": "review_sentence", "model": "qwen-2b",
        "document": id, "sentence": sentence, "cachedOnly": true, "requestId": request_id,
    }));
    assert_error(&review(Some(0)), "invalid_argument");
    assert_error(&review(Some(5)), "canceled");
    for request_id in [Some(6), None] {
        let fresh = review(request_id);
        assert_eq!(fresh["ok"], true, "{fresh}");
    }
    let word = call(store.handle, json!({
        "version": 1, "operation": "explain_word", "model": "qwen-2b",
        "document": id, "sentence": sentence, "word": "bateau", "cachedOnly": true, "requestId": 5,
    }));
    assert_eq!(word["ok"], true, "{word}");
}

#[test]
fn full_reset_allows_staged_file_and_pdf_import_on_the_same_handle() {
    let store = StoreFixture::new();
    let pdf = store.dir.join("source.pdf");
    let original = mixed_pdf_fixture();
    std::fs::write(&pdf, &original).unwrap();

    let stage = |request_id, input| call(store.handle, json!({
        "version": 1, "operation": "stage_intake", "requestId": request_id, "input": input,
    }));
    let txt_request = "b122e518-3ffe-4957-8b75-3e0ef1b92bef";
    let pdf_request = "95f32703-4c2e-428b-b057-51e6dea95fef";
    let txt_input = json!({"kind": "file", "name": "source.txt", "bytes": "Le bateau revient.".as_bytes()});
    let pdf_input = json!({"kind": "pdfFile", "name": "source.pdf", "path": pdf});
    assert_eq!(stage(txt_request, txt_input.clone())["ok"], true);
    assert_eq!(stage(pdf_request, pdf_input.clone())["ok"], true);

    let reset = call(store.handle, json!({"version": 1, "operation": "clear_storage", "scope": "all"}));
    assert_eq!(reset["ok"], true, "{reset}");
    let empty = call(store.handle, json!({"version": 1, "operation": "list_intake", "start": 0}));
    assert_eq!(empty["data"], json!([]));

    let new_text = stage(txt_request, txt_input);
    let new_pdf = stage(pdf_request, pdf_input);
    assert_eq!(new_text["ok"], true, "{new_text}");
    assert_eq!(new_pdf["ok"], true, "{new_pdf}");
    let complete = |id, result| call(store.handle, json!({
        "version": 1, "operation": "complete_intake", "id": id, "result": result,
    }));
    let text_done = complete(&new_text["data"]["id"], json!({"kind": "document", "language": "fr"}));
    assert_eq!(text_done["ok"], true, "{text_done}");
    let pdf_id = &new_pdf["data"]["id"];
    let mut pages = vec![""; 42];
    pages[1] = "Premier chapitre.";
    let prepared = call(store.handle, json!({
        "version": 1, "operation": "save_intake_pdf_text", "id": pdf_id,
        "draft": { "pageTexts": pages, "deferredFrom": 2 },
    }));
    assert_eq!(prepared["ok"], true, "{prepared}");
    let pdf_done = complete(pdf_id, json!({"kind": "stagedPdf", "language": "fr"}));
    assert_eq!(pdf_done["ok"], true, "{pdf_done}");
    let file = call(store.handle, json!({
        "version": 1, "operation": "original_file", "id": pdf_done["data"]["documentId"],
    }));
    assert_eq!(file["ok"], true, "{file}");
    assert_eq!(std::fs::read(file["data"]["path"].as_str().unwrap()).unwrap(), original);
}

#[test]
fn recovered_pdf_checkpoint_preserves_saved_pages_without_resolving_the_unread_tail() {
    let mut store = StoreFixture::new();
    let original = store.dir.join("checkpoint.pdf");
    std::fs::write(&original, b"%PDF-1.4\n").unwrap();
    let request = |lines, empty_pages| json!({
        "version": 1, "operation": "import_pdf_file", "name": "checkpoint.pdf", "language": "fr",
        "path": original, "pageTexts": ["Première page.", "", "", ""],
        "lines": lines, "emptyPages": empty_pages, "deferredFrom": 1,
    });
    let first = call(store.handle, request(json!([]), json!([])));
    assert_eq!(first["ok"], true, "{first}");
    let id = &first["data"]["id"];
    let window = |handle| call(handle, json!({
        "version": 1, "operation": "pdf_read_window", "id": id, "center": 2,
    }));
    assert_eq!(window(store.handle)["data"]["pages"][0]["state"], "needsTextLayer");

    let restored = call(store.handle, request(json!([{
        "page": 1, "text": "Une page déjà reconnue.", "x": 0.1, "y": 0.2, "width": 0.8, "height": 0.1,
    }]), json!([2])));
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(&restored["data"]["id"], id);
    store.reopen();
    let window = window(store.handle);
    assert_eq!(window["ok"], true, "{window}");
    let pages = &window["data"]["pages"];
    assert_eq!(pages[0]["state"], "recognized", "{window}");
    assert_eq!(pages[0]["text"], "Une page déjà reconnue.");
    assert_eq!(pages[1]["state"], "empty");
    assert_eq!(pages[2]["state"], "needsTextLayer");
}

#[test]
fn manual_pdf_ocr_retry_resumes_only_the_current_window_without_waiting() {
    let store = StoreFixture::new();
    let original = store.dir.join("scan.pdf");
    std::fs::write(&original, b"%PDF-1.4\n").unwrap();
    let imported = call(store.handle, json!({
        "version": 1, "operation": "import_pdf_file", "name": "scan.pdf", "language": "fr",
        "path": original, "pageTexts": ["", "", ""], "lines": [],
    }));
    assert_eq!(imported["ok"], true, "{imported}");
    let id = &imported["data"]["id"];
    let focused = call(store.handle, json!({
        "version": 1, "operation": "set_pdf_reading_page", "id": id, "page": 1,
    }));
    assert_eq!(focused["ok"], true, "{focused}");
    let claim = |center, retry_now| {
        let reply = call(store.handle, json!({
            "version": 1, "operation": "claim_pdf_ocr_page", "id": id, "center": center, "retryNow": retry_now,
        }));
        assert_eq!(reply["ok"], true, "{reply}");
        reply["data"].clone()
    };
    let fail = |lease: serde_json::Value| {
        let reply = call(store.handle, json!({
            "version": 1, "operation": "retry_pdf_ocr_page", "id": id,
            "page": lease["page"], "token": lease["token"],
        }));
        assert_eq!(reply["data"], true, "{reply}");
    };
    for page in [1, 0, 2] {
        let lease = claim(1, false);
        assert_eq!(lease["page"], page);
        fail(lease);
    }
    assert!(claim(1, false).is_null());
    assert!(claim(0, true).is_null());
    assert!(claim(1, false).is_null());
    let retried = claim(1, true);
    assert_eq!(retried["page"], 1);
    assert!(claim(1, true).is_null());
    fail(retried);
    assert_eq!(claim(1, false)["page"], 0);
}

#[test]
fn canceling_ocr_page_review_leaves_later_requests_available() {
    let store = StoreFixture::new();
    let imported = call(store.handle, json!({
        "version": 1, "operation": "import_scanned_pdf", "name": "scan.pdf", "language": "en",
        "bytes": b"%PDF-1.4\n", "pages": 1,
        "lines": [{"page": 0, "text": "A rnagic book.", "x": 0.1, "y": 0.2, "width": 0.8, "height": 0.1, "confidence": 0.5}],
    }));
    assert_eq!(imported["ok"], true, "{imported}");
    let request = |request_id| call(store.handle, json!({
        "version": 1, "operation": "review_ocr_page", "id": imported["data"]["id"],
        "revisionId": imported["data"]["currentRevisionId"], "page": 0, "detached": false,
        "cachedOnly": true, "requestId": request_id,
    }));
    let canceled = call(store.handle, json!({
        "version": 1, "operation": "cancel_ocr_page_review", "requestId": 1,
    }));
    assert_eq!(canceled["ok"], true, "{canceled}");
    assert_error(&request(1), "canceled");
    assert_eq!(request(2)["ok"], true);
}

#[test]
fn unsupported_versions_and_operations_are_rejected_explicitly() {
    for version in [0, 2, u32::MAX] {
        assert_error(
            &call(0, json!({"version": version, "operation": "snapshot"})),
            "unsupported_version",
        );
    }
    let store = StoreFixture::new();
    assert_error(
        &call(
            store.handle,
            json!({"version": 1, "operation": "delete_everything"}),
        ),
        "invalid_request",
    );
    // Rejected commands leave the existing store usable.
    assert_eq!(
        call(store.handle, json!({"version": 1, "operation": "snapshot"}))["ok"],
        true
    );
}

#[test]
fn owned_responses_survive_other_calls_and_release_exactly_once() {
    let first = Response::call(0, b"{}");
    let first_value = first.json();
    let first_id = first.allocation();
    let second = Response::call(0, br#"{"version":2,"operation":"snapshot"}"#);
    let second_id = second.allocation();
    assert_ne!(first_id, second_id);
    assert_eq!(first.json(), first_value);
    assert_eq!(second.release(), 0);
    assert_eq!(reader_release(second_id), 1);
    assert_eq!(first.json(), first_value);
    assert_eq!(first.release(), 0);
    assert_eq!(reader_release(first_id), 1);
    assert_eq!(reader_release(0), 1);
    let next = Response::call(0, b"{}");
    assert_ne!(next.allocation(), first_id);
    assert_ne!(next.allocation(), second_id);
    assert_eq!(next.release(), 0);
}

#[test]
fn open_snapshot_close_rejects_stale_handles() {
    assert_error(
        &call(0, json!({"version": 1, "operation": "snapshot"})),
        "invalid_handle",
    );
    let store = StoreFixture::new();
    let snapshot = call(store.handle, json!({"version": 1, "operation": "snapshot"}));
    assert_eq!(snapshot["ok"], true, "{snapshot}");
    assert_eq!(snapshot["data"]["documents"], json!([]));
    assert_eq!(snapshot["data"]["vocabulary"], json!([]));
    assert_eq!(reader_close(store.handle), 0);
    assert_eq!(reader_close(store.handle), 1);
    assert_error(
        &call(store.handle, json!({"version": 1, "operation": "snapshot"})),
        "invalid_handle",
    );
    let replacement = StoreFixture::new();
    assert_ne!(replacement.handle, store.handle);
    assert_error(
        &call(store.handle, json!({"version": 1, "operation": "snapshot"})),
        "invalid_handle",
    );
}

#[test]
fn concurrent_close_and_use_complete_with_valid_responses() {
    let store = StoreFixture::new();
    let handle = store.handle;
    let barrier = Arc::new(Barrier::new(3));
    let reader_barrier = barrier.clone();
    let reader = std::thread::spawn(move || {
        reader_barrier.wait();
        for _ in 0..64 {
            let value = call(handle, json!({"version": 1, "operation": "snapshot"}));
            if value["ok"] == true {
                assert!(value["data"]["documents"].is_array());
            } else {
                assert_error(&value, "invalid_handle");
            }
        }
    });
    let closer_barrier = barrier.clone();
    let closer = std::thread::spawn(move || {
        closer_barrier.wait();
        reader_close(handle)
    });
    barrier.wait();
    assert_eq!(closer.join().unwrap(), 0);
    reader.join().unwrap();
    assert_error(
        &call(handle, json!({"version": 1, "operation": "snapshot"})),
        "invalid_handle",
    );
}

#[test]
fn model_cancellation_can_arrive_first_and_is_scoped_to_the_store() {
    let store = StoreFixture::new();
    let other = StoreFixture::new();
    let cancel = |request_id| {
        call(store.handle, json!({
            "version": 1, "operation": "cancel_model_translation", "requestId": request_id,
        }))
    };
    let translate = |handle, request_id| {
        call(handle, json!({
            "version": 1, "operation": "translate_with_model", "requestId": request_id,
            "id": "missing-document", "sentenceId": "missing-sentence", "target": "uk",
        }))
    };

    assert_error(&cancel(0), "invalid_argument");
    assert_eq!(cancel(2)["ok"], true);
    assert_eq!(cancel(1)["ok"], true);
    assert_error(&translate(store.handle, 2), "canceled");
    assert_error(&translate(store.handle, 3), "core_error");
    assert_error(&translate(other.handle, 2), "core_error");
}

#[test]
fn canceling_speech_prefetch_does_not_cancel_active_speech_or_qwen() {
    let store = StoreFixture::new();
    let cancel = |prefetch| call(store.handle, json!({
        "version": 1, "operation": "cancel_speech", "requestId": 3, "prefetch": prefetch,
    }));
    let speech = |operation, request_id| call(store.handle, json!({
        "version": 1, "operation": operation, "requestId": request_id,
        "id": "missing-document", "sentenceId": "missing-sentence", "speaker": 0, "rate": 1.0,
    }));
    assert_eq!(cancel(true)["ok"], true);
    assert_error(&speech("prefetch_speech", 3), "canceled");
    assert_error(&speech("synthesize_speech", 3), "core_error");
    assert_error(&call(store.handle, json!({
        "version": 1, "operation": "translate_with_model", "requestId": 3,
        "id": "missing-document", "sentenceId": "missing-sentence", "target": "uk",
    })), "core_error");
    assert_eq!(cancel(false)["ok"], true);
    assert_error(&speech("synthesize_speech", 3), "canceled");
    assert_error(&speech("synthesize_speech", 4), "core_error");
}

#[test]
fn bookmarks_are_idempotent_and_survive_restart_and_restore() {
    let mut store = StoreFixture::new();
    let imported = call(
        store.handle,
        json!({
            "version": 1, "operation": "import", "name": "book.txt",
            "language": "en", "bytes": b"First sentence. Second sentence."
        }),
    );
    assert_eq!(imported["ok"], true, "{imported}");
    let doc = &imported["data"];
    let id = &doc["id"];
    let sentence_id = &doc["sentences"][1]["id"];
    assert!(sentence_id.is_string(), "{doc}");

    let set = |handle, saved| {
        call(
            handle,
            json!({
                "version": 1, "operation": "set_bookmark", "id": id, "sentenceId": sentence_id, "saved": saved
            }),
        )
    };
    assert_eq!(set(store.handle, true)["ok"], true);
    assert_eq!(set(store.handle, true)["ok"], true);
    assert_error(
        &call(
            store.handle,
            json!({
                "version": 1, "operation": "set_bookmark", "id": id, "sentenceId": "missing", "saved": true
            }),
        ),
        "core_error",
    );
    let moved = call(
        store.handle,
        json!({
            "version": 1, "operation": "position", "id": id, "position": 1
        }),
    );
    assert_eq!(moved["ok"], true, "{moved}");
    let snapshot = |handle| call(handle, json!({"version": 1, "operation": "snapshot"}));
    store.reopen();
    let exported = call(store.handle, json!({"version": 1, "operation": "export"}));
    assert_eq!(exported["ok"], true, "{exported}");
    let backup = &exported["data"];
    let restore = |handle, backup| {
        call(
            handle,
            json!({
                "version": 1, "operation": "restore", "backup": backup
            }),
        )
    };

    let target = StoreFixture::new();
    assert_eq!(restore(target.handle, backup.clone())["ok"], true);
    let restored = snapshot(target.handle);
    assert_eq!(
        restored["data"]["documents"][0]["bookmarks"],
        json!([sentence_id])
    );
    assert_eq!(restored["data"]["documents"][0]["position"], 1);
    assert_eq!(set(target.handle, false)["ok"], true);
    assert_eq!(set(target.handle, false)["ok"], true);
    assert_eq!(
        snapshot(target.handle)["data"]["documents"][0]["bookmarks"],
        json!([])
    );

    let mut legacy = backup.clone();
    legacy["snapshot"]["documents"][0]
        .as_object_mut()
        .unwrap()
        .remove("bookmarks");
    let legacy_target = StoreFixture::new();
    assert_eq!(restore(legacy_target.handle, legacy)["ok"], true);
    assert_eq!(
        snapshot(legacy_target.handle)["data"]["documents"][0]["bookmarks"],
        json!([])
    );

    let mut invalid = backup.clone();
    invalid["snapshot"]["documents"][0]["bookmarks"] = json!(["missing"]);
    let invalid_target = StoreFixture::new();
    assert_error(&restore(invalid_target.handle, invalid), "core_error");
    assert_eq!(
        snapshot(invalid_target.handle)["data"]["documents"],
        json!([])
    );
}

#[test]
fn reviewed_translations_follow_stable_segments_and_target_languages() {
    let mut store = StoreFixture::new();
    let imported = call(
        store.handle,
        json!({
            "version": 1, "operation": "import", "name": "story.txt",
            "language": "fr", "bytes": b"Bonjour. Bonne nuit."
        }),
    );
    assert_eq!(imported["ok"], true, "{imported}");
    let doc = &imported["data"];
    let id = &doc["id"];
    let first = doc["sentences"][0]["id"].as_str().unwrap();
    let second = doc["sentences"][1]["id"].as_str().unwrap();
    let set = |handle, sentence, target, text| {
        call(
            handle,
            json!({
                "version": 1, "operation": "set_reviewed_translation",
                "id": id, "sentenceId": sentence, "target": target, "text": text
            }),
        )
    };
    assert_eq!(set(store.handle, first, "uk", " Вітаю! ")["ok"], true);
    assert_eq!(set(store.handle, second, "uk", "На добраніч.")["ok"], true);
    assert_eq!(set(store.handle, first, "en", "Hello.")["ok"], true);
    assert_error(&set(store.handle, first, "fr", "Salut."), "core_error");
    assert_error(
        &set(store.handle, "missing", "uk", "Невідоме."),
        "core_error",
    );
    let oversized = "😀".repeat(2001);
    assert_error(&set(store.handle, first, "uk", &oversized), "core_error");

    store.reopen();
    let exported = call(store.handle, json!({"version": 1, "operation": "export"}));
    assert_eq!(exported["ok"], true, "{exported}");
    let backup = &exported["data"];
    let translations = &backup["snapshot"]["documents"][0]["reviewedTranslations"];
    assert_eq!(translations["uk"][first], "Вітаю!");
    assert_eq!(translations["uk"][second], "На добраніч.");
    assert_eq!(translations["en"][first], "Hello.");
    let target = StoreFixture::new();
    let restore = |handle, backup| {
        call(
            handle,
            json!({
                "version": 1, "operation": "restore", "backup": backup
            }),
        )
    };
    assert_eq!(restore(target.handle, backup.clone())["ok"], true);
    assert_eq!(set(target.handle, first, "uk", " ")["ok"], true);
    let snapshot = call(
        target.handle,
        json!({"version": 1, "operation": "snapshot"}),
    );
    let remaining = &snapshot["data"]["documents"][0]["reviewedTranslations"];
    assert!(remaining["uk"].get(first).is_none());
    assert_eq!(remaining["uk"][second], "На добраніч.");
    assert_eq!(remaining["en"][first], "Hello.");

    let mut invalid = backup.clone();
    invalid["snapshot"]["documents"][0]["reviewedTranslations"]["uk"]["other"] = json!("Невідоме.");
    let invalid_target = StoreFixture::new();
    assert_error(&restore(invalid_target.handle, invalid), "core_error");
    let snapshot = call(
        invalid_target.handle,
        json!({"version": 1, "operation": "snapshot"}),
    );
    assert_eq!(snapshot["data"]["documents"], json!([]));
}

#[test]
fn photo_import_preserves_original_ocr_geometry_and_rebuilds_offsets_on_restore() {
    let store = StoreFixture::new();
    // Valid PNG. OCR results are supplied by the platform adapter and tested here as input data.
    let png = hex_png_fixture();
    let lines = json!([
        {"text":"  Bonjour 🦊.  ","x":0.04622069468642864,"y":0.18456221956844754,"width":0.5683778490339008,"height":0.20831482227031994,"confidence":0.9},
        {"text":"Вітаю, світе!","x":0.1,"y":0.4,"width":0.6,"height":0.1,"confidence":null},
    ]);
    let import = |lines| {
        call(
            store.handle,
            json!({
                "version":1,"operation":"import_image","name":"photo.png","language":"fr",
                "bytes":png,"lines":lines
            }),
        )
    };
    let mut invalid = lines.clone();
    invalid[0]["width"] = json!(1.0);
    assert_error(&import(invalid), "core_error");
    let imported = import(lines.clone());
    assert_eq!(imported["ok"], true, "{imported}");
    let document = &imported["data"];
    let id = &document["id"];
    assert_eq!(document["text"], "Bonjour 🦊.\nВітаю, світе!");
    let mut projected = lines.clone();
    projected[0]["sourceSpan"] = json!({"startUtf8":0,"endUtf8":13,"startUtf16":0,"endUtf16":11});
    projected[1]["sourceSpan"] = json!({"startUtf8":14,"endUtf8":37,"startUtf16":12,"endUtf16":25});
    assert_eq!(document["ocrLines"], projected);
    assert_eq!(import(lines.clone())["data"]["id"], *id);
    let saved = call(store.handle, json!({"version":1,"operation":"export"}));
    let mut backup = saved["data"].clone();
    backup["snapshot"]["documents"][0]["text"] = json!("Forged derived text");
    backup["snapshot"]["documents"][0]["sentences"] = json!([]);

    let target = StoreFixture::new();
    let restored = call(
        target.handle,
        json!({"version":1,"operation":"restore","backup":backup}),
    );
    assert_eq!(restored["ok"], true, "{restored}");
    let snapshot = call(target.handle, json!({"version":1,"operation":"snapshot"}));
    let copy = &snapshot["data"]["documents"][0];
    assert_eq!(copy["text"], document["text"]);
    assert_eq!(copy["sentences"], document["sentences"]);
    assert_eq!(copy["ocrLines"], projected);
    let original = call(
        target.handle,
        json!({"version":1,"operation":"original","id":id}),
    );
    assert_eq!(original["data"]["bytes"], json!(png));
}

fn hex_png_fixture() -> Vec<u8> {
    let encoded = "89504e470d0a1a0a0000000d4948445200000001000000010804000000b51c0c020000000b4944415478da6364f80f00010501012718e3660000000049454e44ae426082";
    (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16).unwrap())
        .collect()
}

#[test]
fn scanned_pdf_pages_survive_restore_and_invalid_page_geometry_is_rejected() {
    let mut store = StoreFixture::new();
    // Only the PDF signature is inspected by this boundary. Page rendering and OCR run in the platform adapter.
    let source = b"%PDF-1.7\n%%EOF\n";
    let recognized = json!([
        {"page":0,"text":"First scanned page.","x":0.12,"y":0.2,"width":0.5,"height":0.1,"confidence":0.9},
        {"page":40,"text":"Last scanned page.","x":0.12,"y":0.4,"width":0.6,"height":0.1,"confidence":0.9},
    ]);
    let import = |lines, pages| {
        call(
            store.handle,
            json!({
                "version":1,"operation":"import_scanned_pdf","name":"scan.pdf","language":"en",
                "bytes":source,"lines":lines,"pages":pages,
            }),
        )
    };
    assert_error(&import(recognized.clone(), 40), "core_error");
    assert_error(&import(recognized.clone(), 0), "core_error");
    let reversed = json!([recognized[1], recognized[0]]);
    assert_error(&import(reversed, 41), "core_error");

    let imported = import(recognized, 41);
    assert_eq!(imported["ok"], true, "{imported}");
    let document = &imported["data"];
    assert_eq!(document["ocrPdfPages"], 41);
    assert_eq!(
        document["text"],
        "First scanned page.\n\nLast scanned page."
    );
    assert_eq!(document["ocrLines"][1]["page"], 40);
    let id = document["id"].as_str().unwrap();
    let position = call(store.handle, json!({
        "version":1,"operation":"set_pdf_reading_page","id":id,"page":38,
    }));
    assert_eq!(position["ok"], true, "{position}");
    store.reopen();
    let reopened = call(store.handle, json!({"version":1,"operation":"snapshot"}));
    assert_eq!(reopened["data"]["pdfReadingPositions"][id], 38);
    assert_eq!(reopened["data"]["documents"][0], *document);

    let backup = call(store.handle, json!({"version":1,"operation":"export"}));
    let target = StoreFixture::new();
    let restored = call(
        target.handle,
        json!({
            "version":1,"operation":"restore","backup":backup["data"],
        }),
    );
    assert_eq!(restored["ok"], true, "{restored}");
    let snapshot = call(target.handle, json!({"version":1,"operation":"snapshot"}));
    assert_eq!(snapshot["data"]["documents"][0], *document);
    assert_eq!(snapshot["data"]["pdfReadingPositions"][id], 38);
    let original = call(
        target.handle,
        json!({
            "version":1,"operation":"original","id":document["id"],
        }),
    );
    assert_eq!(original["data"]["bytes"], json!(source));
}

#[test]
fn mixed_pdf_uses_embedded_text_and_keeps_existing_annotations() {
    let store = StoreFixture::new();
    let pdf = mixed_pdf_fixture();
    let inspect = |handle| {
        call(
            handle,
            json!({
                "version":1,"operation":"pdf_pages_needing_ocr","name":"mixed.pdf","bytes":pdf,"pages":42
            }),
        )
    };
    let missing: Vec<_> = (0..42).filter(|&page| page != 1).collect();
    assert_eq!(inspect(store.handle)["data"], json!(missing));

    let old = call(
        store.handle,
        json!({
            "version":1,"operation":"import","name":"mixed.pdf","bytes":pdf,"language":"en"
        }),
    );
    assert_eq!(old["ok"], true, "{old}");
    let document = &old["data"];
    let id = &document["id"];
    let embedded_sentence = &document["sentences"][0]["id"];
    let bookmark = call(
        store.handle,
        json!({
            "version":1,"operation":"set_bookmark","id":id,"sentenceId":embedded_sentence,"saved":true
        }),
    );
    assert_eq!(bookmark["ok"], true, "{bookmark}");
    let note = call(
        store.handle,
        json!({
            "version":1,"operation":"set_reviewed_translation","id":id,
            "sentenceId":embedded_sentence,"target":"uk","text":"Текстовий шар."
        }),
    );
    assert_eq!(note["ok"], true, "{note}");

    let scan = |page| {
        call(
            store.handle,
            json!({
                "version":1,"operation":"import_mixed_pdf","name":"mixed.pdf","bytes":pdf,"language":"en","pages":42,
                "lines":[{"page":page,"text":"Scanned first page.","x":0.1,"y":0.1,"width":0.7,"height":0.1,"confidence":null,
                    "words":[{"text":"first","x":0.3,"y":0.1,"width":0.2,"height":0.1,"rawStartUtf16":8}]},
                    {"page":41,"text":"Scanned last page.","x":0.1,"y":0.1,"width":0.7,"height":0.1,"confidence":null}]
            }),
        )
    };
    assert_error(&scan(1), "core_error");
    let mixed = scan(0);
    assert_eq!(mixed["ok"], true, "{mixed}");
    assert_eq!(mixed["data"]["id"], *id);
    assert_eq!(
        mixed["data"]["text"],
        "Scanned first page.\n\nEmbedded second page.\n\nScanned last page."
    );
    let mut starts = vec![None; 42];
    starts[0] = Some(0);
    starts[1] = Some(21);
    starts[41] = Some(44);
    assert_eq!(mixed["data"]["pdfPageStarts"], json!(starts));
    assert_eq!(
        mixed["data"]["ocrLines"][0]["words"][0]["sourceSpan"],
        json!({"startUtf8":8,"endUtf8":13,"startUtf16":8,"endUtf16":13})
    );
    assert_eq!(mixed["data"]["position"], 1);
    let moved = mixed["data"]["sentences"][1]["id"].as_str().unwrap();
    assert_eq!(mixed["data"]["bookmarks"], json!([moved]));
    assert_eq!(
        mixed["data"]["reviewedTranslations"]["uk"][moved],
        "Текстовий шар."
    );

    let exported = call(store.handle, json!({"version":1,"operation":"export"}));
    let mut backup = exported["data"].clone();
    backup["snapshot"]["documents"][0]["pdfPageStarts"] = json!([17, 18]);
    let restored = StoreFixture::new();
    let result = call(
        restored.handle,
        json!({"version":1,"operation":"restore","backup":backup}),
    );
    assert_eq!(result["ok"], true, "{result}");
    let current = call(restored.handle, json!({"version":1,"operation":"snapshot"}));
    assert_eq!(current["data"]["documents"][0], mixed["data"]);
}

fn mixed_pdf_fixture() -> Vec<u8> {
    use std::io::Write;
    let content = "BT /F1 24 Tf 50 700 Td (Embedded second page.) Tj ET";
    let kids: Vec<_> = (7..47).map(|object| format!("{object} 0 R")).collect();
    let blank_page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>";
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [3 0 R 4 0 R {}] /Count 42 >>", kids.join(" ")),
        blank_page.to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    objects.extend((0..40).map(|_| blank_page.to_owned()));
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        writeln!(&mut pdf, "{} 0 obj\n{object}\nendobj", index + 1).unwrap();
    }
    let xref = pdf.len();
    let size = objects.len() + 1;
    writeln!(&mut pdf, "xref\n0 {size}\n0000000000 65535 f ").unwrap();
    for offset in offsets {
        writeln!(&mut pdf, "{offset:010} 00000 n ").unwrap();
    }
    writeln!(
        &mut pdf,
        "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF"
    )
    .unwrap();
    pdf
}

#[test]
fn ocr_correction_preserves_raw_recognition_original_and_untouched_annotations() {
    let store = StoreFixture::new();
    let png = hex_png_fixture();
    let imported = call(
        store.handle,
        json!({
            "version":1, "operation":"import_image","name":"photo.png","language":"en","bytes":png,
            "lines":[
                {"text":"Ths is a scn.","x":0.1,"y":0.1,"width":0.7,"height":0.15,"confidence":0.42},
                {"text":"Saved sentence.","x":0.1,"y":0.3,"width":0.7,"height":0.15,"confidence":0.9}
            ]
        }),
    );
    assert_eq!(imported["ok"], true, "{imported}");
    let doc = &imported["data"];
    let id = &doc["id"];
    let aid = &doc["sentences"][1]["id"];
    let save = call(
        store.handle,
        json!({
            "version":1, "operation":"set_reviewed_translation","id":id,
            "sentenceId":aid,"target":"uk","text":"Збережене речення."
        }),
    );
    assert_eq!(save["ok"], true, "{save}");
    let edit = |handle, index, correction| {
        call(
            handle,
            json!({
                "version":1,"operation":"correct_ocr_line","id":id,"index":index,"correction":correction,
            }),
        )
    };
    assert_error(&edit(store.handle, 0, json!("Invalid\nline")), "core_error");
    assert_error(
        &edit(store.handle, 1, json!("Conflicting annotated sentence.")),
        "core_error",
    );
    assert_eq!(edit(store.handle, 0, json!("This is a scan."))["ok"], true);
    let snapshot = |handle| call(handle, json!({"version":1,"operation":"snapshot"}));
    let changed = snapshot(store.handle);
    let changed = &changed["data"]["documents"][0];
    assert_eq!(changed["text"], "This is a scan.\nSaved sentence.");
    assert_eq!(changed["ocrLines"][0]["text"], "Ths is a scn.");
    assert_eq!(changed["ocrLines"][0]["correction"], "This is a scan.");
    assert_eq!(changed["ocrLines"][0]["confidence"], 0.42);
    let moved = changed["sentences"][1]["id"].as_str().unwrap();
    assert_eq!(
        changed["reviewedTranslations"]["uk"][moved],
        "Збережене речення."
    );

    let archived = call(store.handle, json!({"version":1,"operation":"export"}));
    let other = StoreFixture::new();
    let restored = call(
        other.handle,
        json!({"version":1,"operation":"restore","backup":archived["data"]}),
    );
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(snapshot(other.handle)["data"]["documents"][0], *changed);
    let original = call(
        other.handle,
        json!({"version":1,"operation":"original","id":id}),
    );
    assert_eq!(original["data"]["bytes"], json!(png));
    assert_eq!(edit(other.handle, 0, serde_json::Value::Null)["ok"], true);
    assert_eq!(
        snapshot(other.handle)["data"]["documents"][0]["text"],
        doc["text"]
    );
}
