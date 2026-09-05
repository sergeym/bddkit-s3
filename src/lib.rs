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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A handle is an index into this table, never a pointer.
/// Instances are held behind `Arc` so a step never runs while this lock is
/// held: dispatch clones the `Arc` out and releases the guard before touching
/// the network. Holding it across an S3 round-trip would serialise every
/// parallel feature file behind one mutex, and a panic mid-step would poison
/// the table for the rest of the run.
static INSTANCES: Mutex<Option<HashMap<u64, Arc<client::Instance>>>> = Mutex::new(None);
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
    serde_json::json!({
        "name": "s3",
        "version": env!("CARGO_PKG_VERSION"),
        "groups": ["s3"],
        "concurrency": "shared",
        // Keyed by group because a manifest may claim several; this one claims
        // only `s3`. Describing a group the manifest does not claim fails the
        // load.
        "fields": { "s3": config::fields_json() },
    })
    .to_string()
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

/// Optional, and the live counterpart of `validate_config`: the one export
/// allowed to open a connection. Never called during a run — a person or a
/// script asks it through `bddkit doctor --live`, so it may be slow and may
/// need the network, the two things `validate_config` must never be.
///
/// Stateless by contract: no handle, no `init_instance` before it and no
/// `drop_instance` after. The client `probe` builds is dropped before this
/// returns, and `INSTANCES` is never touched.
///
/// A host that predates this contract simply never resolves the symbol, which
/// is reported as "not available" rather than as a failure.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_probe_config(request: *const c_char) -> *mut c_char {
    guard("envelope", move || {
        let raw = input(request);
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => return serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
        };
        // The same parse as `validate_config`, so a typo is still reported as a
        // typo here rather than as an unreachable server.
        let config = match config::InstanceConfig::parse(&value["config"]) {
            Ok(c) => c,
            Err(error) => return serde_json::json!({"ok": false, "error": error}).to_string(),
        };
        match client::Instance::probe(&config) {
            Ok(()) => r#"{"ok":true}"#.to_string(),
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
            .insert(handle, Arc::new(instance));
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

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_dispatch(
    handle: u64,
    step_index: u32,
    request: *const c_char,
) -> *mut c_char {
    guard("dispatch", move || {
        let raw = input(request);
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                return serde_json::json!({"status": "fatal", "error": e.to_string()}).to_string();
            }
        };
        let parsed = steps::Request::parse(&value);
        // The guard is dropped before the step runs. `route` performs S3
        // round-trips, and holding the instance table across one would
        // serialise every parallel feature file behind this single mutex and
        // let a panic mid-step poison it for the rest of the run.
        let instance = {
            let mut table = INSTANCES.lock().expect("instances");
            table.get_or_insert_with(HashMap::new).get(&handle).cloned()
        };
        let Some(instance) = instance else {
            return serde_json::json!({"status": "fatal", "error": "unknown handle"}).to_string();
        };
        steps::route(&instance, step_index, &parsed)
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
        assert_eq!(
            v["name"], "s3",
            "the lock file entry must name this plugin `s3`"
        );
        assert_eq!(v["groups"], serde_json::json!(["s3"]));
        assert_eq!(
            v["concurrency"], "shared",
            "the plugin holds no per-scenario state, so it must not ask for per_worker"
        );
        assert!(
            v["version"].is_string(),
            "a manifest without a version fails the load"
        );
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn every_step_is_anchored_and_declares_a_known_kind() {
        let raw = crate::steps_json();
        let steps: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("steps are JSON");
        assert_eq!(
            steps.len(),
            22,
            "the dispatch match and this table must stay in step"
        );
        for (index, step) in steps.iter().enumerate() {
            let pattern = step["pattern"].as_str().expect("pattern is a string");
            assert!(
                pattern.starts_with('^'),
                "step {index} is not anchored: {pattern}"
            );
            assert!(
                pattern.ends_with('$'),
                "step {index} is not anchored: {pattern}"
            );
            assert_eq!(
                step["group"], "s3",
                "step {index} claims a group the manifest does not"
            );
            let kind = step["kind"].as_str().expect("kind is a string");
            assert!(
                kind == "action" || kind == "assertion",
                "step {index} has kind {kind}"
            );
        }
    }
}

#[cfg(test)]
mod config_contract_tests {
    use serde_json::Value;

    fn declared() -> Vec<Value> {
        let raw = crate::manifest_json();
        let v: Value = serde_json::from_str(&raw).expect("manifest is JSON");
        v["fields"]["s3"]
            .as_array()
            .expect("the manifest declares fields for the s3 group")
            .clone()
    }

    fn a_valid_body() -> Value {
        serde_json::json!({
            "endpoint": "http://localhost:9000",
            "bucket": "acme-backups",
            "access_key": "k",
            "secret_key": "s"
        })
    }

    /// Both directions at once, because both are bugs the contract names. A key
    /// described but rejected by `parse` fails only in a user's suite, since the
    /// host prints `fields` and validates nothing against it; a key accepted but
    /// undescribed cannot be set by `bddkit resource add --<field>` at all, only
    /// through `--json`. `fields_json` derives one from the other, so what this
    /// catches is somebody writing a key straight into `manifest_json` instead.
    #[test]
    fn the_manifest_describes_exactly_the_keys_the_parser_accepts() {
        let described: Vec<String> = declared()
            .iter()
            .map(|f| {
                f["name"]
                    .as_str()
                    .expect("a field name is a string")
                    .to_string()
            })
            .collect();
        let accepted: Vec<String> = crate::config::known_keys().map(str::to_string).collect();
        assert_eq!(described, accepted);
    }

    /// `required` is a claim about `parse`'s behaviour, and nothing but this
    /// test connects the two: a key marked required that `parse` happily
    /// defaults, or an optional one it refuses to do without, misleads whoever
    /// reads `bddkit resource fields` in exactly the way the listing exists to
    /// prevent.
    #[test]
    fn the_required_flag_matches_what_the_parser_actually_demands() {
        for field in declared() {
            let name = field["name"].as_str().expect("string");
            let required = field["required"].as_bool().unwrap_or(false);
            let mut body = a_valid_body();
            body.as_object_mut().expect("object").remove(name);
            let accepted = crate::config::InstanceConfig::parse(&body).is_ok();
            assert_eq!(
                !accepted,
                required,
                "{name:?} is declared required={required}, but parsing without it \
                 {}",
                if accepted { "succeeds" } else { "fails" }
            );
        }
    }

    /// Round-trips the export the way the host does: a JSON request in, a C
    /// string out, freed with the plugin's own free.
    fn probe(request: &str) -> Value {
        let c_request = std::ffi::CString::new(request).expect("no NUL");
        let reply = crate::bddkit_probe_config(c_request.as_ptr());
        assert!(
            !reply.is_null(),
            "returning NULL is an error in the contract"
        );
        let text = unsafe { std::ffi::CStr::from_ptr(reply) }
            .to_string_lossy()
            .into_owned();
        unsafe { crate::bddkit_free_string(reply) };
        serde_json::from_str(&text).expect("the reply is JSON")
    }

    /// The whole point of the second entry point: a config `validate_config`
    /// accepts, because it is well-formed, that the probe still refuses because
    /// nothing answers there.
    #[test]
    fn a_probe_of_a_well_formed_config_that_reaches_nothing_fails_with_a_message() {
        let request = r#"{"group":"s3","instance":"main","config":{
            "endpoint":"http://127.0.0.1:1","bucket":"acme-backups",
            "access_key":"k","secret_key":"s"},"options":{}}"#;
        let validated = {
            let c = std::ffi::CString::new(request).expect("no NUL");
            let reply = crate::bddkit_validate_config(c.as_ptr());
            let text = unsafe { std::ffi::CStr::from_ptr(reply) }
                .to_string_lossy()
                .into_owned();
            unsafe { crate::bddkit_free_string(reply) };
            serde_json::from_str::<Value>(&text).expect("JSON")
        };
        assert_eq!(
            validated["ok"], true,
            "validate_config is offline and must accept this: it is well-formed"
        );

        let probed = probe(request);
        assert_eq!(probed["ok"], false);
        assert!(
            probed["error"]
                .as_str()
                .expect("a failure carries a message")
                .contains("127.0.0.1:1"),
            "the host prints this and nothing else: {}",
            probed["error"]
        );
    }

    /// The probe parses the config the same way, so a typo is still a typo here
    /// — reported as a config error, not as an unreachable server.
    #[test]
    fn a_probe_of_a_malformed_config_reports_the_config_not_the_network() {
        let probed =
            probe(r#"{"group":"s3","instance":"main","config":{"bukcet":"typo"},"options":{}}"#);
        assert_eq!(probed["ok"], false);
        assert!(
            probed["error"]
                .as_str()
                .expect("message")
                .contains("bukcet"),
            "{}",
            probed["error"]
        );
    }

    /// Stateless by contract: no handle is returned and nothing is left behind
    /// for `drop_instance` to release.
    ///
    /// Reads the global `INSTANCES`, which is sound only because no test in
    /// this binary calls `bddkit_init_instance` — tests share one process and
    /// run in threads. A test that registers an instance must give this one a
    /// different way to prove the table was untouched.
    #[test]
    fn a_probe_returns_no_handle_and_registers_no_instance() {
        let probed = probe(
            r#"{"group":"s3","instance":"main","config":{
                "endpoint":"http://127.0.0.1:1","bucket":"acme-backups",
                "access_key":"k","secret_key":"s"},"options":{}}"#,
        );
        assert!(
            probed.get("handle").is_none(),
            "a handle would make every caller run a three-call lifecycle"
        );
        assert!(
            crate::INSTANCES
                .lock()
                .expect("instances")
                .as_ref()
                .is_none_or(|table| table.is_empty()),
            "the probe must not store the client it built"
        );
    }

    /// The gap `the_manifest_describes_exactly_the_keys_the_parser_accepts`
    /// cannot see. Deriving `known_keys` from `FIELDS` closed the drift between
    /// the two lists, but it opened a quieter one: adding a `Field` now makes
    /// `parse` *accept* the key whether or not `parse` reads it. Such a key
    /// lists in `bddkit resource fields`, is accepted into a config, and then
    /// does nothing at all — silently, which is the failure issue #6 names.
    /// So this asserts the value actually arrives, for every optional key
    /// (the required ones are covered by parsing without them).
    #[test]
    fn every_optional_key_actually_reaches_the_parsed_config() {
        let with_all = crate::config::InstanceConfig::parse(&serde_json::json!({
            "endpoint": "http://localhost:9000",
            "bucket": "acme-backups",
            "access_key": "k",
            "secret_key": "s",
            "region": "eu-central-1",
            "url_style": "virtual-hosted",
            "fixtures_dir": "features/files"
        }))
        .expect("valid");
        // Each value differs from the default, so a key that is accepted and
        // then dropped leaves the default behind and fails here.
        assert_eq!(
            with_all.region, "eu-central-1",
            "region was accepted and ignored"
        );
        assert_eq!(
            with_all.url_style,
            crate::config::UrlStyle::VirtualHosted,
            "url_style was accepted and ignored"
        );
        assert_eq!(
            with_all.fixtures_dir.as_deref(),
            Some("features/files"),
            "fixtures_dir was accepted and ignored"
        );
        // The count is the tripwire: a new optional key added to FIELDS lands
        // here as a failure, which is the moment to give it an assertion above.
        let optional = crate::config::optional_key_count();
        assert_eq!(
            optional, 3,
            "an optional key was added to FIELDS; assert here that it reaches InstanceConfig"
        );
    }

    /// The contract is explicit: a non-string `example` fails the host's parse
    /// of the whole manifest, and the plugin then does not load at all, with a
    /// message that never mentions `example`. A key whose value is a boolean or
    /// a number is the trap here.
    ///
    /// `fields_json` builds every example as a string, so today this cannot
    /// fail; it is a guard against a future edit that renders one from a typed
    /// value, and the consequence is severe enough to keep the guard standing.
    #[test]
    fn every_example_is_a_string_even_for_a_boolean_key() {
        for field in declared() {
            let name = field["name"].as_str().expect("string");
            if let Some(example) = field.get("example") {
                assert!(
                    example.is_string(),
                    "{name:?} has a non-string example {example}; the host refuses the manifest"
                );
            }
        }
    }
}
