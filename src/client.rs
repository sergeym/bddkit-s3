//! The S3 client and the instance that owns it.

use crate::config::InstanceConfig;
use s3::{Bucket, Region, creds::Credentials};

// Scaffolding: the fields and the two alternative-credential constructors have
// no reader until Task 9 routes dispatch into `steps.rs`. Remove this attribute
// then — a stale allow hides the next real dead-code warning.
#[allow(dead_code)]
pub struct Instance {
    pub bucket: Box<Bucket>,
    pub region: Region,
    pub access_key: String,
    pub path_style: bool,
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
    path_style: bool,
) -> Result<Box<Bucket>, String> {
    let bucket = Bucket::new(name, region, credentials)
        .map_err(|e| format!("cannot build a client for bucket {name:?}: {e}"))?;
    Ok(if path_style { bucket.with_path_style() } else { bucket })
}

#[allow(dead_code)]
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
                config.path_style,
            )?,
            region,
            access_key: config.access_key.clone(),
            path_style: config.path_style,
            fixtures_dir: config.fixtures_dir.clone(),
        })
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
            self.path_style,
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
            self.path_style,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::InstanceConfig;

    fn cfg(path_style: bool) -> InstanceConfig {
        InstanceConfig::parse(&serde_json::json!({
            "endpoint": "http://localhost:9000",
            "bucket": "acme-backups",
            "access_key": "k",
            "secret_key": "s",
            "path_style": path_style
        }))
        .expect("valid")
    }

    #[test]
    fn a_bucket_is_built_without_opening_a_connection() {
        // No MinIO is running in a unit test. Construction must still succeed:
        // `init_instance` is where connections would be opened, and nothing
        // here opens one.
        let instance = Instance::connect(&cfg(true)).expect("built");
        assert_eq!(instance.bucket.name(), "acme-backups");
    }

    #[test]
    fn path_style_is_reflected_in_the_url_minio_will_see() {
        let path = Instance::connect(&cfg(true)).expect("built");
        assert!(
            path.bucket.url().contains("localhost:9000/acme-backups"),
            "path style puts the bucket in the path: {}",
            path.bucket.url()
        );
    }

    #[test]
    fn foreign_credentials_produce_a_different_signature_than_the_configured_ones() {
        let instance = Instance::connect(&cfg(true)).expect("built");
        let mine = instance.bucket.presign_get("k.txt", 60, None).expect("presign");
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
