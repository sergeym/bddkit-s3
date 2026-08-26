//! bddkit plugin serving the `s3` resource group. Written against
//! docs/plugin-authoring.md; it must never need the host's source.

mod client;
mod config;
mod files;
mod reply;
mod steps;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// A handle is an index into this table, never a pointer.
static INSTANCES: Mutex<Option<HashMap<u64, client::Instance>>> = Mutex::new(None);
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Outside `guard`, and safe there only because it provably cannot panic:
/// `CString::new` returns a `Result` and the fallback literal has no interior
/// NUL. Anything added here must keep that property.
fn out(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| {
            CString::new("{\"ok\":false,\"error\":\"NUL in reply\"}").expect("literal")
        })
        .into_raw()
}

/// Inside `guard` at every call site.
fn input(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// A panic must never unwind across the FFI boundary. Every export is guarded,
/// including the trivial ones: "every export is guarded" is an invariant a
/// reader checks in one pass.
fn guard(envelope_kind: &str, body: impl FnOnce() -> String) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(reply) => out(reply),
        Err(_) => out(match envelope_kind {
            "dispatch" => r#"{"status":"fatal","error":"the plugin panicked"}"#.to_string(),
            _ => r#"{"ok":false,"error":"the plugin panicked"}"#.to_string(),
        }),
    }
}

fn manifest_json() -> String {
    r#"{"name":"s3","version":"0.1.0","groups":["s3"],"concurrency":"shared"}"#.to_string()
}

/// **The index of a step in this array is its identity** — it is the `u32` the
/// host passes to `bddkit_dispatch`. The `match` in `bddkit_dispatch` names the
/// same indices in the same order; keep the two adjacent so they cannot drift.
fn steps_json() -> String {
    serde_json::json!([
        { "pattern": r#"^I upload "([^"]+)" with:$"#,                                  "group": "s3", "kind": "action" },
        { "pattern": r#"^I upload file "([^"]+)" to "([^"]+)"$"#,                      "group": "s3", "kind": "action" },
        { "pattern": r#"^I upload file "([^"]+)" to "([^"]+)" with:$"#,                "group": "s3", "kind": "action" },
        { "pattern": r#"^I upload saved file "([^"]+)" to "([^"]+)"$"#,                "group": "s3", "kind": "action" },
        { "pattern": r#"^I download "([^"]+)" as "([^"]+)"$"#,                         "group": "s3", "kind": "action" },
        { "pattern": r#"^I save "([^"]+)" as "([^"]+)"$"#,                             "group": "s3", "kind": "action" },
        { "pattern": r#"^I read "([^"]+)" of "([^"]+)" as "([^"]+)"$"#,                "group": "s3", "kind": "action" },
        { "pattern": r#"^I delete object "([^"]+)"$"#,                                 "group": "s3", "kind": "action" },
        { "pattern": r#"^I delete all objects under "([^"]+)"$"#,                      "group": "s3", "kind": "action" },
        { "pattern": r#"^I count objects under "([^"]+)" as "([^"]+)"$"#,              "group": "s3", "kind": "action" },
        { "pattern": r#"^I presign a "(GET|PUT)" url for "([^"]+)" valid for "(\d+)" seconds as "([^"]+)"$"#, "group": "s3", "kind": "action" },
        { "pattern": r#"^object "([^"]+)" should exist$"#,                             "group": "s3", "kind": "assertion" },
        { "pattern": r#"^object "([^"]+)" should not exist$"#,                         "group": "s3", "kind": "assertion" },
        { "pattern": r#"^object "([^"]+)" should contain "([^"]+)"$"#,                 "group": "s3", "kind": "assertion" },
        { "pattern": r#"^object "([^"]+)" should equal:$"#,                            "group": "s3", "kind": "assertion" },
        { "pattern": r#"^object "([^"]+)" should have size "(\d+)"$"#,                 "group": "s3", "kind": "assertion" },
        { "pattern": r#"^object "([^"]+)" should have content type "([^"]+)"$"#,       "group": "s3", "kind": "assertion" },
        { "pattern": r#"^object "([^"]+)" should have metadata "([^"]+)" equal to "([^"]*)"$"#, "group": "s3", "kind": "assertion" },
        { "pattern": r#"^there should be "(\d+)" objects under "([^"]+)"$"#,           "group": "s3", "kind": "assertion" },
        { "pattern": r#"^objects under "([^"]+)" should contain "([^"]+)"$"#,          "group": "s3", "kind": "assertion" },
        { "pattern": r#"^anonymous access to "([^"]+)" should be denied$"#,            "group": "s3", "kind": "assertion" },
        { "pattern": r#"^a presigned url for "([^"]+)" with secret "([^"]+)" should be rejected$"#, "group": "s3", "kind": "assertion" }
    ])
    .to_string()
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_abi_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_manifest() -> *mut c_char {
    guard("envelope", manifest_json)
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_list_steps() -> *mut c_char {
    guard("envelope", steps_json)
}

/// Eager, at startup, for every declared instance, with nothing connected.
/// Rejecting here is what turns a config typo into exit 2 before the first
/// request instead of a failure halfway through the suite.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_validate_config(request: *const c_char) -> *mut c_char {
    guard("envelope", move || {
        let raw = input(request);
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => return serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
        };
        match config::InstanceConfig::parse(&value["config"]) {
            Ok(_) => r#"{"ok":true}"#.to_string(),
            Err(error) => serde_json::json!({"ok": false, "error": error}).to_string(),
        }
    })
}

/// Lazy: the first time a scenario runs an `s3` step with this instance
/// selected. Under `shared` that is once for the whole run.
///
/// Every call must return a handle distinct from every live one: the host
/// initialises without holding its lock, so two workers can arrive here at the
/// same time and the loser's handle is dropped while the winner's stays in use.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_init_instance(request: *const c_char) -> *mut c_char {
    guard("envelope", move || {
        let raw = input(request);
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => return serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
        };
        let config = match config::InstanceConfig::parse(&value["config"]) {
            Ok(c) => c,
            Err(error) => return serde_json::json!({"ok": false, "error": error}).to_string(),
        };
        let instance = match client::Instance::connect(&config) {
            Ok(i) => i,
            Err(error) => return serde_json::json!({"ok": false, "error": error}).to_string(),
        };
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
        INSTANCES
            .lock()
            .expect("instances")
            .get_or_insert_with(HashMap::new)
            .insert(handle, instance);
        serde_json::json!({"ok": true, "handle": handle}).to_string()
    })
}

/// Called for every initialised instance at the end of the run, on every exit
/// path including a failed run and `--fail-fast`. Nothing here outlives the
/// process, but the table entry is released so a leaked handle cannot be
/// reused.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_drop_instance(handle: u64) -> *mut c_char {
    guard("envelope", move || {
        INSTANCES
            .lock()
            .expect("instances")
            .get_or_insert_with(HashMap::new)
            .remove(&handle);
        r#"{"ok":true}"#.to_string()
    })
}

/// # Safety
/// `s` must be a pointer this library returned and has not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bddkit_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_manifest_claims_the_s3_group_and_is_shared() {
        let raw = crate::manifest_json();
        let v: serde_json::Value = serde_json::from_str(&raw).expect("manifest is JSON");
        assert_eq!(v["name"], "s3", "the lock file entry must name this plugin `s3`");
        assert_eq!(v["groups"], serde_json::json!(["s3"]));
        assert_eq!(
            v["concurrency"], "shared",
            "the plugin holds no per-scenario state, so it must not ask for per_worker"
        );
        assert!(v["version"].is_string(), "a manifest without a version fails the load");
    }

    #[test]
    fn every_step_is_anchored_and_declares_a_known_kind() {
        let raw = crate::steps_json();
        let steps: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("steps are JSON");
        assert_eq!(steps.len(), 22, "the dispatch match and this table must stay in step");
        for (index, step) in steps.iter().enumerate() {
            let pattern = step["pattern"].as_str().expect("pattern is a string");
            assert!(pattern.starts_with('^'), "step {index} is not anchored: {pattern}");
            assert!(pattern.ends_with('$'), "step {index} is not anchored: {pattern}");
            assert_eq!(step["group"], "s3", "step {index} claims a group the manifest does not");
            let kind = step["kind"].as_str().expect("kind is a string");
            assert!(
                kind == "action" || kind == "assertion",
                "step {index} has kind {kind}"
            );
        }
    }
}
