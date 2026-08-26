//! The instance body from `resources.s3.<name>`. The host has no schema for it
//! and never looks inside, so every check a typo could trip is here.

/// Written by hand rather than derived: `serde(deny_unknown_fields)` would
/// report the failure in serde's words, and this error text is what a tester
/// reads on exit 2. The host prefixes it with `resources.s3.<instance>: `.
#[derive(Debug)]
pub struct InstanceConfig {
    pub endpoint: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
    pub path_style: bool,
    pub fixtures_dir: Option<String>,
}

const KNOWN: &[&str] = &[
    "endpoint",
    "bucket",
    "access_key",
    "secret_key",
    "region",
    "path_style",
    "fixtures_dir",
];

fn required_string(v: &serde_json::Value, key: &str) -> Result<String, String> {
    match v.get(key) {
        Some(serde_json::Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(serde_json::Value::String(_)) => Err(format!("\"{key}\" must not be empty")),
        Some(_) => Err(format!("\"{key}\" must be a string")),
        None => Err(format!("requires a string \"{key}\"")),
    }
}

fn optional_string(v: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("\"{key}\" must be a string")),
    }
}

/// The subset of the S3 naming rules a typo actually trips. Not a substitute
/// for the server's own validation — it exists so `Acme_Backups` fails at
/// startup rather than as a puzzling 400 mid-suite.
fn bucket_name_is_valid(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
}

impl InstanceConfig {
    pub fn parse(config: &serde_json::Value) -> Result<Self, String> {
        let object = config
            .as_object()
            .ok_or_else(|| "the instance body must be a mapping".to_string())?;
        if let Some(unknown) = object.keys().find(|k| !KNOWN.contains(&k.as_str())) {
            return Err(format!(
                "unknown key \"{unknown}\"; known keys are {}",
                KNOWN.join(", ")
            ));
        }

        let endpoint = required_string(config, "endpoint")?;
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Err(format!(
                "\"endpoint\" must start with http:// or https://, got {endpoint:?}"
            ));
        }
        let bucket = required_string(config, "bucket")?;
        if !bucket_name_is_valid(&bucket) {
            return Err(format!(
                "\"bucket\" {bucket:?} is not a valid bucket name (3-63 chars, lowercase letters, digits, - and .)"
            ));
        }

        let path_style = match config.get("path_style") {
            None | Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::Bool(b)) => *b,
            Some(_) => return Err("\"path_style\" must be true or false".to_string()),
        };

        Ok(Self {
            endpoint,
            bucket,
            access_key: required_string(config, "access_key")?,
            secret_key: required_string(config, "secret_key")?,
            region: optional_string(config, "region")?.unwrap_or_else(|| "us-east-1".to_string()),
            path_style,
            fixtures_dir: optional_string(config, "fixtures_dir")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(extra: &str) -> serde_json::Value {
        let mut v = serde_json::json!({
            "endpoint": "http://localhost:9000",
            "bucket": "acme-backups",
            "access_key": "k",
            "secret_key": "s"
        });
        if !extra.is_empty() {
            let more: serde_json::Value = serde_json::from_str(extra).expect("extra is JSON");
            for (k, val) in more.as_object().expect("object") {
                v[k] = val.clone();
            }
        }
        v
    }

    #[test]
    fn a_minimal_config_is_accepted_and_defaults_are_applied() {
        let cfg = InstanceConfig::parse(&body("")).expect("valid");
        assert_eq!(cfg.region, "us-east-1");
        assert!(
            cfg.path_style,
            "MinIO is path style, so that is the default"
        );
        assert_eq!(cfg.fixtures_dir, None);
    }

    #[test]
    fn an_unknown_key_is_rejected_by_name() {
        let error = InstanceConfig::parse(&body(r#"{"bukcet": "typo"}"#)).expect_err("rejected");
        assert!(
            error.contains("bukcet"),
            "the host never looks inside the instance body, so only this check can name the typo: {error}"
        );
    }

    #[test]
    fn a_missing_required_key_is_named() {
        let mut v = body("");
        v.as_object_mut().expect("object").remove("bucket");
        let error = InstanceConfig::parse(&v).expect_err("rejected");
        assert!(error.contains("bucket"), "{error}");
    }

    #[test]
    fn an_endpoint_that_is_not_http_is_rejected() {
        let error = InstanceConfig::parse(&body(r#"{"endpoint": "localhost:9000"}"#))
            .expect_err("rejected");
        assert!(error.contains("endpoint"), "{error}");
    }

    #[test]
    fn a_syntactically_invalid_bucket_name_is_rejected() {
        let error = InstanceConfig::parse(&body(r#"{"bucket": "No_Caps"}"#)).expect_err("rejected");
        assert!(error.contains("bucket"), "{error}");
    }

    #[test]
    fn a_wrongly_typed_optional_key_is_rejected() {
        let error = InstanceConfig::parse(&body(r#"{"path_style": "yes"}"#)).expect_err("rejected");
        assert!(error.contains("path_style"), "{error}");
    }

    #[test]
    fn an_empty_required_value_is_rejected_rather_than_accepted_as_blank() {
        // A key present but blank is the shape `secret_key: ${S3_SECRET}` takes
        // when the variable is unset, so this is the likeliest real failure.
        for key in ["endpoint", "bucket", "access_key", "secret_key"] {
            let mut v = body("");
            v[key] = serde_json::json!("");
            let error = InstanceConfig::parse(&v).expect_err("rejected");
            assert!(error.contains(key), "{key}: {error}");
        }
    }

    #[test]
    fn every_required_key_is_named_when_it_is_missing() {
        for key in ["endpoint", "bucket", "access_key", "secret_key"] {
            let mut v = body("");
            v.as_object_mut().expect("object").remove(key);
            let error = InstanceConfig::parse(&v).expect_err("rejected");
            assert!(error.contains(key), "{key}: {error}");
        }
    }

    #[test]
    fn supplied_optional_values_override_the_defaults() {
        let cfg = InstanceConfig::parse(&body(
            r#"{"region": "eu-central-1", "fixtures_dir": "features/files", "path_style": false}"#,
        ))
        .expect("valid");
        assert_eq!(cfg.region, "eu-central-1");
        assert_eq!(cfg.fixtures_dir.as_deref(), Some("features/files"));
        assert!(
            !cfg.path_style,
            "a real S3 endpoint needs virtual-host style"
        );
    }
}
