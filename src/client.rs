//! The S3 client and the instance that owns it.

use crate::config::{InstanceConfig, UrlStyle};
use s3::{Bucket, Region, creds::Credentials};

/// Short on purpose: see [`Instance::probe`]. Long enough for a loaded server
/// on a slow link, short enough that a firewalled endpoint answers in seconds.
const PROBE_TIMEOUT_SECS: u64 = 5;

/// A failed round trip, told apart where the message would otherwise mislead.
///
/// A wrong `region` is the case worth catching. AWS answers a bucket asked for
/// in the wrong region with `301` carrying `x-amz-bucket-region` and **no**
/// `Location`; `attohttpc` follows redirects, finds no header to follow, and
/// reports a transport failure. So this never reaches the status match in
/// [`Instance::probe`], and left alone it reads as "cannot reach" — the one
/// thing that is not wrong, since the endpoint answered and even said where the
/// bucket really lives.
///
/// ponytail: matched on the client's message, because the response is gone by
/// the time this sees it. `rust-s3` exposes no way to turn redirect-following
/// off, so the alternative is a wrapper around its request layer; do that if
/// this ever needs the region out of the header rather than a guess at it.
fn transport_error(config: &InstanceConfig, error: &impl std::fmt::Display) -> String {
    let text = error.to_string();
    if text.contains("location header") {
        return format!(
            "{endpoint} redirected bucket {bucket:?} without saying where, which is how AWS \
             answers a bucket that lives in another region — {region:?} is probably not its \
             region ({text})",
            endpoint = config.endpoint,
            bucket = config.bucket,
            region = config.region
        );
    }
    format!(
        "cannot reach {endpoint}: {text}",
        endpoint = config.endpoint
    )
}

// `bucket` and `fixtures_dir` are now read from `steps.rs`.
pub struct Instance {
    pub bucket: Box<Bucket>,
    pub region: Region,
    pub access_key: String,
    /// Kept only so `foreign_signature_should_be_rejected` can refuse to run
    /// when the tester passed this bucket's own secret — that request would
    /// succeed regardless of whether a genuinely foreign signature is
    /// rejected, so comparing here is what keeps the assertion honest.
    pub secret_key: String,
    pub url_style: UrlStyle,
    pub fixtures_dir: Option<String>,
}

fn region_of(config: &InstanceConfig) -> Region {
    Region::Custom {
        region: config.region.clone(),
        endpoint: config.endpoint.clone(),
    }
}

fn bucket_for(
    name: &str,
    region: Region,
    credentials: Credentials,
    url_style: UrlStyle,
) -> Result<Box<Bucket>, String> {
    let bucket = Bucket::new(name, region, credentials)
        .map_err(|e| format!("cannot build a client for bucket {name:?}: {e}"))?;
    // The one place the choice loses its name: `rust-s3` expresses it as the
    // presence or absence of a call, so this is the boundary the enum exists to
    // reach without becoming a bool any earlier.
    Ok(match url_style {
        UrlStyle::Path => bucket.with_path_style(),
        UrlStyle::VirtualHosted => bucket,
    })
}

impl Instance {
    /// Opens nothing: `Bucket` is a description of where to send requests, and
    /// the first request is sent by the first step. A MinIO that is down is
    /// therefore a step failure, not a load failure — which is right, because
    /// the host initialises lazily and a run of API-only tests must not need S3.
    pub fn connect(config: &InstanceConfig) -> Result<Self, String> {
        let credentials = Credentials::new(
            Some(&config.access_key),
            Some(&config.secret_key),
            None,
            None,
            None,
        )
        .map_err(|e| format!("cannot build credentials: {e}"))?;
        let region = region_of(config);
        Ok(Self {
            bucket: bucket_for(
                &config.bucket,
                region.clone(),
                credentials,
                config.url_style,
            )?,
            region,
            access_key: config.access_key.clone(),
            secret_key: config.secret_key.clone(),
            url_style: config.url_style,
            fixtures_dir: config.fixtures_dir.clone(),
        })
    }

    /// The live half of the config contract, and the one call in this plugin
    /// allowed to open a connection. Stateless: nothing is stored, nothing is
    /// returned but the verdict, and the client is dropped on the way out.
    ///
    /// One `HEAD` at the bucket root, chosen because it is the only call whose
    /// three failures stay distinguishable. `list_page` parses every response
    /// body as `ListBucketResult` regardless of status, so an S3 error document
    /// collapses into a generic deserialization `Err` and the status is gone
    /// before it can be read — the same trap `keys_under` documents in
    /// `steps.rs`. `exists()` asks for the account's whole bucket list, which a
    /// key scoped to one bucket may not have, and would report a bucket that is
    /// right there as missing.
    ///
    /// Each arm names the failure rather than the layer, because that text is
    /// the entire value of this call: `doctor --live` prints it and nothing
    /// else.
    pub fn probe(config: &InstanceConfig) -> Result<(), String> {
        let instance = Self::connect(config)?;
        // `connect` keeps the crate default of 60s, which is right for a step
        // moving an object. Here it is wrong twice over: `rust-s3` retries once
        // with a second's pause, so an endpoint that swallows packets rather
        // than refusing them — a closed security group, typically — would hang
        // `doctor --live` for two minutes. A reachability check that cannot
        // answer quickly has failed at the only thing it does.
        let bucket = instance
            .bucket
            .with_request_timeout(std::time::Duration::from_secs(PROBE_TIMEOUT_SECS))
            .map_err(|e| format!("cannot set the probe timeout: {e}"))?;
        let (_, status) = bucket
            .head_object("")
            .map_err(|e| transport_error(config, &e))?;
        match status {
            200..=299 => Ok(()),
            // A HEAD carries no body, so there is no error code to separate the
            // reasons. At the bucket root most 403s are about who is asking,
            // but a key scoped to objects alone (`s3:GetObject` without
            // `s3:ListBucket`) is refused here while working perfectly for
            // every step — so the message names the likeliest cause without
            // claiming it is the only one.
            403 => Err(format!(
                "{endpoint} refused this request for bucket {bucket:?}: the credentials for \
                 access key {key:?} were rejected, or they may lack access to the bucket",
                endpoint = config.endpoint,
                bucket = config.bucket,
                key = config.access_key
            )),
            // AWS answers 404 both for a bucket that is not there and for one
            // the caller may not see, deliberately, so that a stranger cannot
            // probe for the existence of someone else's bucket. MinIO is
            // franker, but the message must hold for both.
            404 => Err(format!(
                "bucket {bucket:?} was not found at {endpoint}: it may not exist, or these \
                 credentials may not be allowed to see it",
                bucket = config.bucket,
                endpoint = config.endpoint
            )),
            other => Err(format!(
                "{endpoint} answered {other} for bucket {bucket:?}",
                endpoint = config.endpoint,
                bucket = config.bucket
            )),
        }
    }

    /// The same bucket signed with a different secret, for the assertion that
    /// a forged signature is refused.
    pub fn with_secret(&self, secret: &str) -> Result<Box<Bucket>, String> {
        let credentials = Credentials::new(Some(&self.access_key), Some(secret), None, None, None)
            .map_err(|e| format!("cannot build credentials: {e}"))?;
        bucket_for(
            self.bucket.name().as_str(),
            self.region.clone(),
            credentials,
            self.url_style,
        )
    }

    /// The same bucket with no credentials at all, for the assertion that
    /// anonymous access is denied.
    pub fn anonymous(&self) -> Result<Box<Bucket>, String> {
        let credentials =
            Credentials::anonymous().map_err(|e| format!("cannot build credentials: {e}"))?;
        bucket_for(
            self.bucket.name().as_str(),
            self.region.clone(),
            credentials,
            self.url_style,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::InstanceConfig;

    fn cfg(url_style: UrlStyle) -> InstanceConfig {
        InstanceConfig::parse(&serde_json::json!({
            "endpoint": "http://localhost:9000",
            "bucket": "acme-backups",
            "access_key": "k",
            "secret_key": "s",
            "url_style": match url_style {
                UrlStyle::Path => "path",
                UrlStyle::VirtualHosted => "virtual-hosted",
            }
        }))
        .expect("valid")
    }

    #[test]
    fn a_bucket_is_built_without_opening_a_connection() {
        // No MinIO is running in a unit test. Construction must still succeed:
        // `init_instance` is where connections would be opened, and nothing
        // here opens one.
        let instance = Instance::connect(&cfg(UrlStyle::Path)).expect("built");
        assert_eq!(instance.bucket.name(), "acme-backups");
    }

    #[test]
    fn each_url_style_is_reflected_in_the_url_the_server_will_see() {
        let path = Instance::connect(&cfg(UrlStyle::Path)).expect("built");
        assert!(
            path.bucket.url().contains("localhost:9000/acme-backups"),
            "path style puts the bucket in the path: {}",
            path.bucket.url()
        );
        let virtual_hosted = Instance::connect(&cfg(UrlStyle::VirtualHosted)).expect("built");
        assert!(
            virtual_hosted
                .bucket
                .url()
                .contains("acme-backups.localhost:9000"),
            "virtual-hosted style puts the bucket in a subdomain: {}",
            virtual_hosted.bucket.url()
        );
    }

    /// The one probe branch reachable with no server at all. 127.0.0.1:1 refuses
    /// instantly, the same trick the step tests use.
    #[test]
    fn a_probe_of_an_endpoint_nothing_answers_names_the_endpoint() {
        let config = InstanceConfig::parse(&serde_json::json!({
            "endpoint": "http://127.0.0.1:1",
            "bucket": "acme-backups",
            "access_key": "k",
            "secret_key": "s"
        }))
        .expect("valid");
        let error = Instance::probe(&config).expect_err("nothing is listening on port 1");
        assert!(
            error.contains("http://127.0.0.1:1"),
            "the error must name what could not be reached, not the layer that failed: {error}"
        );
        assert!(
            !error.contains("was not found"),
            "an unreachable endpoint must not be reported as a missing bucket: {error}"
        );
    }

    /// A server that answers every request the way AWS answers a bucket asked
    /// for in the wrong region: `301` carrying `x-amz-bucket-region` and **no**
    /// `Location`. Observed against the real thing; reproduced here so the test
    /// needs no network. Returns the endpoint to point a config at.
    fn a_server_that_redirects_without_a_location() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let Ok(mut stream) = stream else { continue };
                use std::io::Write;
                let _ = stream.write_all(
                    b"HTTP/1.1 301 Moved Permanently\r\n\
                      x-amz-bucket-region: us-west-1\r\n\
                      Content-Length: 0\r\n\r\n",
                );
            }
        });
        endpoint
    }

    /// `attohttpc` follows redirects and cannot be told not to through
    /// `rust-s3`, so this answer never reaches the status match — it surfaces as
    /// a transport error about a missing header. Left unhelped, the report then
    /// says "cannot reach", which is the one thing that is not wrong with the
    /// configuration: the endpoint answered, and it even said where the bucket
    /// really lives.
    #[test]
    fn a_redirect_without_a_location_blames_the_region_not_the_endpoint() {
        let endpoint = a_server_that_redirects_without_a_location();
        let config = InstanceConfig::parse(&serde_json::json!({
            "endpoint": endpoint,
            "bucket": "acme-backups",
            "access_key": "k",
            "secret_key": "s",
            "region": "us-east-1"
        }))
        .expect("valid");
        let error = Instance::probe(&config).expect_err("a bare 301 is not success");
        assert!(
            error.contains("region"),
            "the likeliest cause is the region, and the report must say so: {error}"
        );
        assert!(
            error.contains("us-east-1"),
            "naming the configured region is what makes it actionable: {error}"
        );
    }

    /// The three live branches, against a real S3-compatible server. Skips —
    /// printing why, rather than failing — when no server is configured, the
    /// same discipline `tests/e2e.rs` uses for a missing `bddkit` binary. CI
    /// gets its server from `docker-compose.yml`; see the README for driving it
    /// against a MinIO you already run.
    fn live_config(bucket: Option<&str>, secret: Option<&str>) -> Option<InstanceConfig> {
        let endpoint = std::env::var("BDDKIT_S3_ENDPOINT").ok()?;
        let real_bucket = std::env::var("BDDKIT_S3_BUCKET").ok()?;
        let access_key = std::env::var("BDDKIT_S3_ACCESS_KEY").ok()?;
        let real_secret = std::env::var("BDDKIT_S3_SECRET_KEY").ok()?;
        Some(
            InstanceConfig::parse(&serde_json::json!({
                "endpoint": endpoint,
                "bucket": bucket.unwrap_or(&real_bucket),
                "access_key": access_key,
                "secret_key": secret.unwrap_or(&real_secret)
            }))
            .expect("the live config is valid"),
        )
    }

    macro_rules! require_live_s3 {
        ($bucket:expr, $secret:expr) => {
            match live_config($bucket, $secret) {
                Some(c) => c,
                None => {
                    eprintln!(
                        "SKIP: no live S3 configured — set BDDKIT_S3_ENDPOINT, \
                         BDDKIT_S3_BUCKET, BDDKIT_S3_ACCESS_KEY and BDDKIT_S3_SECRET_KEY."
                    );
                    return;
                }
            }
        };
    }

    #[test]
    fn a_probe_of_a_reachable_bucket_succeeds() {
        let config = require_live_s3!(None, None);
        Instance::probe(&config).expect("the configured bucket is reachable");
    }

    #[test]
    fn a_probe_of_a_bucket_that_is_not_there_says_so() {
        let config = require_live_s3!(Some("bddkit-s3-no-such-bucket"), None);
        let error = Instance::probe(&config).expect_err("that bucket does not exist");
        assert!(
            error.contains("was not found"),
            "a missing bucket and refused credentials are different problems: {error}"
        );
    }

    #[test]
    fn a_probe_with_the_wrong_secret_blames_the_credentials() {
        let wrong = "not-the-secret";
        let config = require_live_s3!(None, Some(wrong));
        let error = Instance::probe(&config).expect_err("that secret is wrong");
        // The 403 arm is the one that interpolates config fields, so this is
        // where a leak would actually happen; the offline test below can only
        // reach the transport arm.
        assert!(
            !error.contains(wrong),
            "the secret reached the report: {error}"
        );
        assert!(
            error.contains("were rejected"),
            "a rejected signature must not read as a missing bucket: {error}"
        );
        assert!(
            !error.contains("was not found"),
            "the two failures must not share a phrase: {error}"
        );
    }

    /// `doctor --live` prints this text, and a CI log keeps it. The access key
    /// is an identifier and naming it is what makes the message actionable; the
    /// secret is a credential and must never appear, on any branch. This covers
    /// the transport arm, which needs no server;
    /// `a_probe_with_the_wrong_secret_blames_the_credentials` covers the 403
    /// arm, the only other one that interpolates the config.
    #[test]
    fn no_probe_failure_ever_prints_the_secret_key() {
        let secret = "s3cr3t-that-must-not-leak";
        for (endpoint, bucket) in [
            ("http://127.0.0.1:1", "acme-backups"),
            ("http://127.0.0.1:1", "no-such-bucket"),
        ] {
            let config = InstanceConfig::parse(&serde_json::json!({
                "endpoint": endpoint,
                "bucket": bucket,
                "access_key": "k",
                "secret_key": secret
            }))
            .expect("valid");
            let error = Instance::probe(&config).expect_err("nothing is listening");
            assert!(
                !error.contains(secret),
                "the secret reached the report: {error}"
            );
        }
    }

    #[test]
    fn foreign_credentials_produce_a_different_signature_than_the_configured_ones() {
        let instance = Instance::connect(&cfg(UrlStyle::Path)).expect("built");
        let mine = instance
            .bucket
            .presign_get("k.txt", 60, None)
            .expect("presign");
        let theirs = instance
            .with_secret("not-the-secret")
            .expect("built")
            .presign_get("k.txt", 60, None)
            .expect("presign");
        assert_ne!(
            mine, theirs,
            "the wrong-secret assertion depends on this actually signing differently"
        );
    }
}
