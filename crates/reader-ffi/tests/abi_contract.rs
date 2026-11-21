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
