mod support;

use reader_core::{Backup, MAX_BACKUP_FILE_BYTES};
use serde_json::{json, Value};
use std::fs::{self, File};
use support::{assert_error, call, Response, StoreFixture};

fn invoke(store: &StoreFixture, operation: &str, path: &std::path::Path) -> Value {
    let request =
        serde_json::to_vec(&json!({"version": 1, "operation": operation, "path": path})).unwrap();
    let response = Response::call(store.handle, &request);
    assert_ne!(response.allocation(), 0);
    let value = response.json();
    assert_eq!(response.release(), 0);
    value
}

fn import(store: &StoreFixture, text: &[u8]) -> Value {
    let result = call(
        store.handle,
        json!({"version":1,"operation":"import", "name":"story.txt", "language":"en","bytes":text}),
    );
    assert_eq!(result["ok"], true, "{result}");
    result["data"].clone()
}

#[test]
fn file_backup_matches_legacy_schema_and_restores_across_stores() {
    let source = StoreFixture::new();
    let doc = import(&source, b"First sentence. Second sentence.");
    let moved = call(
        source.handle,
        json!({"version":1,"operation":"position","id":doc["id"],"position":1}),
    );
    assert_eq!(moved["ok"], true);
    let entry = call(
        source.handle,
        json!({"version":1,"operation":"save_word","entry":{
            "id":"","word":"First","translation":"premier","language":"en","targetLanguage":"fr",
            "context":"First sentence.","documentId":doc["id"],
            "forward":{"dueAt":0,"intervalDays":0,"reviews":0},"reverse":{"dueAt":0,"intervalDays":0,"reviews":0}
        }}),
    );
    assert_eq!(entry["ok"], true, "{entry}");
    assert_eq!(
        call(
            source.handle,
            json!({"version":1,"operation":"review","id":entry["data"]["id"],"direction":"reverse","grade":2})
        )["ok"],
        true
    );
    let path = source.dir.join("backup.json");
    let exported = invoke(&source, "export_backup_file", &path);
    assert_eq!(exported, json!({"version":1,"ok":true,"data":null}));
    let typed: Backup = serde_json::from_reader(File::open(&path).unwrap()).unwrap();
    let legacy = call(source.handle, json!({"version":1,"operation":"export"}));
    assert_eq!(serde_json::to_value(typed).unwrap(), legacy["data"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    }
    let mut destination = StoreFixture::new();
    assert_eq!(
        invoke(&destination, "restore_backup_file", &path),
        json!({"version":1,"ok":true,"data":[]})
    );
    destination.reopen();
    let snapshot = call(
        destination.handle,
        json!({"version":1,"operation":"snapshot"}),
    );
    assert_eq!(snapshot["data"]["documents"][0]["position"], 1);
    assert_eq!(
        snapshot["data"]["vocabulary"][0]["reverse"]["fsrs"],
        legacy["data"]["snapshot"]["vocabulary"][0]["reverse"]["fsrs"]
    );
    let original = call(
        destination.handle,
        json!({"version":1,"operation":"original","id":doc["id"]}),
    );
    assert_eq!(
        original["data"]["bytes"],
        json!(b"First sentence. Second sentence.")
    );
    let mut old_json = legacy["data"].clone();
    old_json["resources"] =
        json!([{"kind":"translation","id":"uk-fr","revision":"pinned-fixture"}]);
    let old_path = source.dir.join("legacy.json");
    serde_json::to_writer(File::create(&old_path).unwrap(), &old_json).unwrap();
    let restored = invoke(&destination, "restore_backup_file", &old_path);
    assert_eq!(restored["ok"], true);
    assert_eq!(restored["data"], old_json["resources"]);
}

#[test]
fn file_backup_failures_preserve_existing_files_and_store() {
    let source = StoreFixture::new();
    import(&source, b"First valid document.");
    import(&source, b"Second document.");
    let destination = StoreFixture::new();
    import(&destination, b"Existing private document.");
    let snapshot = || {
        call(
            destination.handle,
            json!({"version":1,"operation":"snapshot"}),
        )
    };
    let before = snapshot();
    let path = source.dir.join("existing.json");
    fs::write(&path, b"keep existing bytes").unwrap();
    assert_error(&invoke(&source, "export_backup_file", &path), "core_error");
    assert_eq!(fs::read(&path).unwrap(), b"keep existing bytes");
    let legacy = call(source.handle, json!({"version":1,"operation":"export"}));
    let mut corrupt = legacy["data"].clone();
    corrupt["originals"][1]["bytes"][0] = json!(0);
    serde_json::to_writer(File::create(&path).unwrap(), &corrupt).unwrap();
    assert_error(
        &invoke(&destination, "restore_backup_file", &path),
        "core_error",
    );
    assert_eq!(snapshot(), before);
    for resources in [
        json!(vec![
            json!({"kind":"optional","id":"piper-uk","revision":"pinned"});
            1025
        ]),
        json!([{"kind":"optional","id":"x".repeat(1025),"revision":"pinned"}]),
    ] {
        let mut huge_manifest = legacy["data"].clone();
        huge_manifest["resources"] = resources;
        serde_json::to_writer(File::create(&path).unwrap(), &huge_manifest).unwrap();
        assert_error(
            &invoke(&destination, "restore_backup_file", &path),
            "core_error",
        );
        assert_eq!(snapshot(), before);
    }
    let oversized = File::create(&path).unwrap();
    oversized.set_len(MAX_BACKUP_FILE_BYTES + 1).unwrap();
    assert_error(
        &invoke(&destination, "restore_backup_file", &path),
        "core_error",
    );
    assert_eq!(snapshot(), before);
    fs::write(&path, b"{\"version\":1").unwrap();
    assert_error(
        &invoke(&destination, "restore_backup_file", &path),
        "core_error",
    );
    assert_eq!(snapshot(), before);
    for operation in ["export_backup_file", "restore_backup_file"] {
        for path in ["".to_owned(), "bad\0path".to_owned(), "x".repeat(4097)] {
            assert_error(
                &call(
                    source.handle,
                    json!({"version":1,"operation":operation,"path":path}),
                ),
                "invalid_argument",
            );
        }
    }
    assert!(fs::read_dir(&source.dir).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".reader-backup-")));
}
