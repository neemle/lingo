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
    assert_eq!(document["ocrLines"], lines);
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
    assert_eq!(copy["ocrLines"], lines);
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
