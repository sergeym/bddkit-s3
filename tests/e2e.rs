//! The end-to-end suite for this plugin: the real `bddkit` binary, this
//! crate's own `cdylib`, and a real MinIO from `docker-compose.yml`.
//! Needs `docker compose up minio-init` first.
//!
//! `bddkit` is the external dependency here (this repository ships only the
//! plugin), so these tests skip themselves — printing why, rather than
//! failing — when no usable binary can be found. See [`bddkit_bin`].

use std::path::{Path, PathBuf};
use std::process::Command;

const ENDPOINT: &str = "http://localhost:9000";
const BUCKET: &str = "apibdd-it";

/// Resolves the `bddkit` binary to run against: `BDDKIT_BIN` env var, then
/// `bddkit` on `PATH`, then a sibling `../bddkit` checkout's debug build.
/// `None` means none of the three is available.
fn bddkit_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BDDKIT_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("bddkit");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let sibling = sibling_debug_bin();
    if sibling.is_file() {
        return Some(sibling);
    }
    None
}

fn sibling_debug_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../bddkit/target/debug/bddkit")
}

/// Returns `true` if the test should proceed; prints an explanation and
/// returns `false` (skip, not fail) otherwise.
macro_rules! require_bddkit {
    () => {
        match bddkit_bin() {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIP: bddkit binary not available — looked for {}. \
                     Set BDDKIT_BIN to its path, put bddkit on PATH, or check \
                     out sergeym/bddkit as a sibling of this repository and \
                     `cargo build` it.",
                    sibling_debug_bin().display()
                );
                return;
            }
        }
    };
}

/// Builds this crate's own `cdylib` the ordinary way (`cargo build` in this
/// repository) and returns the path to the artifact under this repo's own
/// `target/`.
///
/// No `--target-dir` isolation here, unlike the old host-side helper this
/// suite replaces: that isolation existed because the plugin used to be a
/// path-dependency member of the host's own workspace, sharing the host's
/// target directory and Cargo lock with a `cargo test` process that might
/// still be building when the helper's nested `cargo build` ran. Here the
/// test binary and the cdylib are built from the *same* package — by the
/// time `cargo test` starts running test binaries, its own build phase has
/// already finished and released the target-directory lock, so a `cargo
/// build` invoked from inside a test for this same package finds it free
/// and simply confirms nothing needs rebuilding.
fn build_plugin() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(env!("CARGO"))
        .arg("build")
        .current_dir(root)
        .output()
        .expect("failed to run cargo build for the plugin");
    assert!(
        out.status.success(),
        "plugin build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let path = root.join("target/debug").join(format!(
        "{}bddkit_s3{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    assert!(
        path.exists(),
        "plugin artifact missing at {}",
        path.display()
    );
    path
}

/// A throwaway project: config, feature files, and the hand-written lock file
/// the P1 loader reads. `plugin install` is a later milestone.
fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bddkit-s3-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("features")).expect("mkdir features");
    std::fs::create_dir_all(dir.join(".bddkit")).expect("mkdir .bddkit");
    for (file, body) in files {
        std::fs::write(dir.join("features").join(file), body).expect("write feature");
    }
    std::fs::write(
        dir.join(".bddkit/plugins.yaml"),
        format!(
            "plugin:\n  - name: s3\n    path: {}\n",
            build_plugin().display()
        ),
    )
    .expect("write lock");
    std::fs::write(
        dir.join("cfg.yaml"),
        format!(
            "paths: [features]\n\
             concurrency: 1\n\
             resources:\n\
             \x20 api:\n\
             \x20   main:\n\
             \x20     base_url: {ENDPOINT}\n\
             \x20 s3:\n\
             \x20   main:\n\
             \x20     endpoint: {ENDPOINT}\n\
             \x20     bucket: {BUCKET}\n\
             \x20     access_key: bddkit\n\
             \x20     secret_key: bddkit-secret\n"
        ),
    )
    .expect("write config");
    dir
}

fn run(bin: &Path, dir: &Path) -> std::process::Output {
    Command::new(bin)
        .args(["--config", "cfg.yaml"])
        .current_dir(dir)
        .output()
        .expect("failed to run bddkit")
}

/// THE GATE from issue #10: a scenario uploads an object to MinIO and asserts
/// it is present, with the plugin loaded from a hand-written lock file.
#[test]
fn an_object_uploaded_to_minio_is_found_there() {
    let bin = require_bddkit!();
    let feature = r#"Feature: S3 upload
  Scenario: an uploaded object is in the bucket
    Given set variable "p" to "<<unique()>>"
    And I upload "<<p>>/report.pdf" with:
      """
      the quarterly report
      """
    Then object "<<p>>/report.pdf" should exist
"#;
    let dir = project("gate", &[("gate.feature", feature)]);
    let out = run(&bin, &dir);
    assert!(
        out.status.success(),
        "the gate scenario failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Everything the gate does not touch, in one run: metadata, listing,
/// deletion, access refusal — and a presigned URL driven by bddkit's own HTTP
/// steps, which is the only place the plugin layer and the built-in layer are
/// shown working inside one scenario.
#[test]
fn the_rest_of_the_vocabulary_works_against_minio() {
    let bin = require_bddkit!();
    let feature = r#"Feature: S3 vocabulary
  Background:
    Given set variable "p" to "<<unique()>>"

  Scenario: metadata survives a round trip
    Given I upload "<<p>>/a.txt" with:
      """
      alpha
      """
    Then object "<<p>>/a.txt" should contain "alph"
    And object "<<p>>/a.txt" should have size "7"
    When I read "etag" of "<<p>>/a.txt" as "tag"
    Then object "<<p>>/a.txt" should exist

  Scenario: a prefix can be counted and cleared
    Given I upload "<<p>>/b/1.txt" with:
      """
      one
      """
    And I upload "<<p>>/b/2.txt" with:
      """
      two
      """
    Then there should be "2" objects under "<<p>>/b/"
    And objects under "<<p>>/b/" should contain "<<p>>/b/1.txt"
    When I delete all objects under "<<p>>/b/"
    Then there should be "0" objects under "<<p>>/b/"

  Scenario: the bucket is not open to the world
    Given I upload "<<p>>/c.txt" with:
      """
      secret
      """
    Then anonymous access to "<<p>>/c.txt" should be denied
    And a presigned url for "<<p>>/c.txt" with secret "wrong-secret" should be rejected

  Scenario: a presigned url is honoured by the built-in HTTP steps
    Given I upload "<<p>>/d.txt" with:
      """
      delta
      """
    When I presign a "GET" url for "<<p>>/d.txt" valid for "60" seconds as "url"
    And I request "<<url>>" using HTTP GET
    Then the response code is 200
"#;
    let dir = project("vocabulary", &[("vocab.feature", feature)]);
    let out = run(&bin, &dir);
    assert!(
        out.status.success(),
        "the vocabulary run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
