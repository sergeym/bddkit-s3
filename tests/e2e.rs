//! The end-to-end suite for this plugin: the real `bddkit` binary, this
//! crate's own `cdylib`, and a real MinIO from `docker-compose.yml`.
//! Needs `docker compose up minio-init` first.
//!
//! `bddkit` is the external dependency here (this repository ships only the
//! plugin), so these tests skip themselves — printing why, rather than
//! failing — when no usable binary can be found. See [`bddkit_bin`].
//!
//! Two hosts matter, and the suite is written to run against either. CI pins a
//! published bddkit that predates the plugin config contract: it ignores the
//! manifest's `fields` and never resolves `bddkit_probe_config`, so the
//! scenario tests below double as the proof that both additions stay backward
//! compatible — a host that choked on either would fail to load the plugin at
//! all, and every one of them would go red. The three contract tests need a
//! host that reads them — the field listing, the clean probe, and the
//! unreachable endpoint — and say so and skip when it does not.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The defaults are `docker-compose.yml`'s, which is what CI runs. The
/// overrides let the same suite drive an S3 you already have — see the README.
fn endpoint() -> String {
    std::env::var("BDDKIT_S3_ENDPOINT").unwrap_or_else(|_| "http://localhost:9000".to_string())
}

fn bucket() -> String {
    std::env::var("BDDKIT_S3_BUCKET").unwrap_or_else(|_| "apibdd-it".to_string())
}

fn access_key() -> String {
    std::env::var("BDDKIT_S3_ACCESS_KEY").unwrap_or_else(|_| "bddkit".to_string())
}

fn secret_key() -> String {
    std::env::var("BDDKIT_S3_SECRET_KEY").unwrap_or_else(|_| "bddkit-secret".to_string())
}

/// Resolves the `bddkit` binary to run against: `BDDKIT_BIN` env var, then
/// `bddkit` on `PATH`, then a sibling `../bddkit` checkout's build.
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
    // Release before debug: a host built to try a new contract against this
    // plugin is the one just compiled, and `cargo build --release` is what
    // leaves it here.
    sibling_bins().into_iter().find(|p| p.is_file())
}

fn sibling_bins() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    vec![
        root.join("../bddkit/target/release/bddkit"),
        root.join("../bddkit/target/debug/bddkit"),
    ]
}

fn sibling_bins_listed() -> String {
    sibling_bins()
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ")
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
                    sibling_bins_listed()
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
    std::fs::write(dir.join("cfg.yaml"), config_yaml(&endpoint(), &bucket()))
        .expect("write config");
    dir
}

fn config_yaml(endpoint: &str, bucket: &str) -> String {
    format!(
        "paths: [features]\n\
         concurrency: 1\n\
         resources:\n\
         \x20 api:\n\
         \x20   main:\n\
         \x20     base_url: {endpoint}\n\
         \x20 s3:\n\
         \x20   main:\n\
         \x20     endpoint: {endpoint}\n\
         \x20     bucket: {bucket}\n\
         \x20     access_key: {access}\n\
         \x20     secret_key: {secret}\n",
        access = access_key(),
        secret = secret_key()
    )
}

/// The pinned 0.1.1 host takes the config as a bare flag; a host that has grown
/// subcommands wants `run` in front of it. Asked of the binary rather than
/// assumed, so the same suite drives either one.
fn run(bin: &Path, dir: &Path) -> std::process::Output {
    let mut args = vec!["--config", "cfg.yaml"];
    if host_has_subcommands(bin) {
        args.insert(0, "run");
    }
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run bddkit")
}

fn host_has_subcommands(bin: &Path) -> bool {
    host_help(bin).contains("Commands:")
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

/// What this host can be asked, read once from its top-level `--help`.
///
/// Deliberately not probed with `bddkit resource fields --help`: on the CLI
/// 0.1.1 shipped, `resource` and `fields` are positional paths and `--help`
/// still exits 0, so every such probe answers yes on every host. The help text
/// is the only place the two shapes actually differ.
fn host_help(bin: &Path) -> String {
    Command::new(bin)
        .arg("--help")
        .output()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_default()
}

/// False for the host CI pins, which predates the plugin config contract: it
/// ignores the manifest's `fields` and never resolves `bddkit_probe_config`.
/// Asked of the binary rather than of a version number, because a version says
/// nothing about which commit it was cut from.
fn host_reads_the_config_contract(bin: &Path) -> bool {
    host_lists_subcommand(bin, "resource")
}

/// Looks inside the `Commands:` block rather than anywhere in the help, so a
/// host that merely mentions "resources" in a description is not mistaken for
/// one that has the subcommand.
fn host_lists_subcommand(bin: &Path, name: &str) -> bool {
    host_help(bin)
        .split_once("Commands:")
        .is_some_and(|(_, commands)| {
            commands
                .lines()
                .filter_map(|line| line.split_whitespace().next())
                .any(|word| word == name)
        })
}

macro_rules! require_contract_host {
    ($bin:expr) => {
        if !host_reads_the_config_contract(&$bin) {
            eprintln!(
                "SKIP: {} predates `bddkit resource fields`, so it neither reads the \
                 manifest's `fields` nor calls `bddkit_probe_config`. Point BDDKIT_BIN \
                 at a newer host to run this.",
                $bin.display()
            );
            return;
        }
    };
}

/// The manifest half of the contract, seen from the outside: the keys a person
/// configuring an `s3` instance can discover without reading this source.
#[test]
fn the_host_lists_every_key_this_plugin_declares() {
    let bin = require_bddkit!();
    require_contract_host!(bin);
    let dir = project("fields", &[("f.feature", TRIVIAL_FEATURE)]);
    let out = Command::new(&bin)
        .args(["resource", "fields", "s3", "--config", "cfg.yaml"])
        .current_dir(&dir)
        .output()
        .expect("failed to run bddkit resource fields");
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "listing the fields failed:\n{listing}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Matched at the start of a line, not as a substring: the host prints
    // `  <name padded><required><description>`, and several descriptions
    // mention other keys by name — "secret access key paired with access_key"
    // would satisfy a `contains("access_key")` with the field itself missing.
    let names: Vec<&str> = listing
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    for key in [
        "endpoint",
        "bucket",
        "access_key",
        "secret_key",
        "region",
        "url_style",
        "fixtures_dir",
    ] {
        assert!(
            names.contains(&key),
            "{key} is not listed as a field of its own:\n{listing}"
        );
    }
    assert!(
        !listing.contains("does not describe its fields"),
        "the host found no `fields` in the manifest:\n{listing}"
    );
}

/// The probe half, end to end: the host resolves the optional symbol, calls it,
/// and reports what it answered.
#[test]
fn the_host_probes_this_plugins_resource_and_reports_it_clean() {
    let bin = require_bddkit!();
    require_contract_host!(bin);
    let dir = project("probe-ok", &[("p.feature", TRIVIAL_FEATURE)]);
    let report = doctor_live(&bin, &dir);
    assert!(
        report.contains("probed clean"),
        "the probe did not run or did not succeed:\n{report}"
    );
    assert!(
        !report.contains("exports no bddkit_probe_config"),
        "the host did not resolve the probe symbol:\n{report}"
    );
}

/// The error text is the entire value of the probe — `doctor --live` prints it
/// and nothing else — so this pins that an unreachable endpoint reads as an
/// unreachable endpoint, and never as a missing bucket.
#[test]
fn a_probe_of_an_unreachable_endpoint_says_what_it_could_not_reach() {
    let bin = require_bddkit!();
    require_contract_host!(bin);
    let dir = project("probe-dead", &[("p.feature", TRIVIAL_FEATURE)]);
    // Rewritten after `project` wrote the working one: 127.0.0.1:1 refuses
    // instantly, so this stays fast and needs no server.
    std::fs::write(
        dir.join("cfg.yaml"),
        config_yaml("http://127.0.0.1:1", &bucket()),
    )
    .expect("write config");
    let report = doctor_live(&bin, &dir);
    assert!(
        report.contains("cannot reach http://127.0.0.1:1"),
        "the report must name what could not be reached:\n{report}"
    );
    assert!(
        !report.contains("was not found"),
        "an unreachable endpoint must not be reported as a missing bucket:\n{report}"
    );
}

const TRIVIAL_FEATURE: &str = "Feature: a suite doctor can check\n\
                               \x20 Scenario: nothing is asserted\n\
                               \x20   Given set variable \"a\" to \"b\"\n";

/// `doctor` reports problems through its exit code, and a dead endpoint is
/// meant to be one, so the status is deliberately not asserted here — the
/// report itself is what these tests read.
fn doctor_live(bin: &Path, dir: &Path) -> String {
    let out = Command::new(bin)
        .args(["doctor", "--config", "cfg.yaml", "--live"])
        .current_dir(dir)
        .output()
        .expect("failed to run bddkit doctor");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
