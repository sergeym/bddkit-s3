//! The instance body from `resources.s3.<name>`. The host has no schema for it
//! and never looks inside, so every check a typo could trip is here.

/// Which of the two forms S3 defines a request URL takes. A named type rather
/// than a `bool` so that the config key, this enum and every call site say the
/// same word; it becomes a boolean only at the `rust-s3` boundary, which is the
/// one place the distinction genuinely has no name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlStyle {
    /// `host/bucket/key` — the bucket in the path, which is what MinIO serves.
    Path,
    /// `bucket.host/key` — the bucket as a subdomain, which is what AWS prefers.
    VirtualHosted,
}

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
    pub url_style: UrlStyle,
    pub fixtures_dir: Option<String>,
}

/// One key of an instance body, as `parse` enforces it and as the manifest
/// describes it. The four fields are exactly what the plugin contract allows —
/// deliberately not JSON Schema, because the host prints these and interprets
/// none of them.
struct Field {
    name: &'static str,
    required: bool,
    /// Carries what the four keys cannot express as structure: that a key has a
    /// default, that it needs a scheme, which values it accepts.
    description: &'static str,
    /// Always rendered as a JSON string, including for a key whose value is a
    /// boolean or a number: a non-string `example` fails the host's parse of the
    /// whole manifest, and the plugin then does not load at all.
    example: Option<&'static str>,
}

/// **The single list.** `parse` accepts a key because it is here, and
/// `manifest_json` describes it because it is here, so a key added to this
/// table cannot be accepted-but-undescribed or described-but-rejected — the
/// two failures the plugin contract warns about. `required` is a claim about
/// `parse`'s behaviour that `config_contract_tests` checks by actually parsing.
const FIELDS: &[Field] = &[
    Field {
        name: "endpoint",
        required: true,
        description: "S3-compatible endpoint, including the scheme (http:// or https://)",
        example: Some("http://localhost:9000"),
    },
    Field {
        name: "bucket",
        required: true,
        description: "the one bucket these steps read and write; one instance is one bucket",
        example: Some("acceptance"),
    },
    Field {
        name: "access_key",
        required: true,
        description: "access key id the steps sign requests with",
        example: Some("minioadmin"),
    },
    Field {
        name: "secret_key",
        required: true,
        description: "secret access key paired with access_key",
        example: Some("minioadmin"),
    },
    Field {
        name: "region",
        required: false,
        description: "region sent in the signature; defaults to us-east-1",
        example: Some("eu-central-1"),
    },
    Field {
        name: "url_style",
        required: false,
        description: "where the bucket goes in the URL: \"path\" (the default) puts it in the \
                      path, host/bucket/key, which is what MinIO serves; \"virtual-hosted\" \
                      puts it in a subdomain, bucket.host/key, which is what AWS prefers",
        example: Some("virtual-hosted"),
    },
    Field {
        name: "fixtures_dir",
        required: false,
        description: "base directory the `upload file` steps resolve a local path against; \
                      required only by those steps",
        example: Some("features/files"),
    },
];

/// Every key an instance body may carry. `parse` and the manifest both read
/// this, so a key added to `FIELDS` is accepted and described in one edit.
pub fn known_keys() -> impl Iterator<Item = &'static str> {
    FIELDS.iter().map(|f| f.name)
}

/// How many keys carry a default rather than being demanded. Read only by the
/// test that asserts each of them survives into `InstanceConfig`.
#[cfg(test)]
pub fn optional_key_count() -> usize {
    FIELDS.iter().filter(|f| !f.required).count()
}

/// The `fields` entry the manifest carries for the `s3` group. A field with no
/// example omits the key rather than sending it empty, because the contract
/// makes it optional.
pub fn fields_json() -> serde_json::Value {
    FIELDS
        .iter()
        .map(|f| {
            let mut entry = serde_json::json!({
                "name": f.name,
                "required": f.required,
                "description": f.description,
            });
            if let Some(example) = f.example {
                entry["example"] = serde_json::Value::String(example.to_string());
            }
            entry
        })
        .collect()
}

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
        if let Some(unknown) = object
            .keys()
            .find(|k| !known_keys().any(|known| known == k.as_str()))
        {
            return Err(format!(
                "unknown key \"{unknown}\"; known keys are {}",
                known_keys().collect::<Vec<_>>().join(", ")
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

        // Named rather than boolean, for two reasons. A wrong value can then say
        // what the right ones are — a boolean key could only ever answer
        // "must be true or false" — and the host writes every plugin field as a
        // YAML string, so a boolean-only key could never be set through
        // `bddkit resource add --<field>` at all.
        let url_style = match config.get("url_style") {
            None | Some(serde_json::Value::Null) => UrlStyle::Path,
            Some(serde_json::Value::String(s)) if s == "path" => UrlStyle::Path,
            Some(serde_json::Value::String(s)) if s == "virtual-hosted" => UrlStyle::VirtualHosted,
            Some(other) => {
                return Err(format!(
                    "\"url_style\" must be \"path\" or \"virtual-hosted\", got {other}"
                ));
            }
        };

        Ok(Self {
            endpoint,
            bucket,
            access_key: required_string(config, "access_key")?,
            secret_key: required_string(config, "secret_key")?,
            region: optional_string(config, "region")?.unwrap_or_else(|| "us-east-1".to_string()),
            url_style,
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
        assert_eq!(
            cfg.url_style,
            UrlStyle::Path,
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
        let error = InstanceConfig::parse(&body(r#"{"region": true}"#)).expect_err("rejected");
        assert!(error.contains("region"), "{error}");
    }

    /// The two addressing forms S3 defines, named rather than spelled as a
    /// boolean: `path` puts the bucket in the path (`host/bucket/key`, which is
    /// what MinIO serves), `virtual-hosted` puts it in a subdomain
    /// (`bucket.host/key`, which is what AWS prefers).
    #[test]
    fn url_style_selects_the_addressing_form_by_name() {
        assert_eq!(
            InstanceConfig::parse(&body("")).expect("valid").url_style,
            UrlStyle::Path,
            "path is the default, because the suite's own MinIO serves that form"
        );
        assert_eq!(
            InstanceConfig::parse(&body(r#"{"url_style": "path"}"#))
                .expect("valid")
                .url_style,
            UrlStyle::Path
        );
        assert_eq!(
            InstanceConfig::parse(&body(r#"{"url_style": "virtual-hosted"}"#))
                .expect("valid")
                .url_style,
            UrlStyle::VirtualHosted
        );
    }

    /// The reason this key is named rather than boolean: a wrong value can say
    /// what the right ones are. A boolean key could only ever answer
    /// "must be true or false".
    #[test]
    fn an_unknown_url_style_lists_the_forms_that_exist() {
        let error = InstanceConfig::parse(&body(r#"{"url_style": "virtualhosted"}"#))
            .expect_err("rejected");
        assert!(error.contains("url_style"), "{error}");
        assert!(
            error.contains("path"),
            "the error must name both forms: {error}"
        );
        assert!(
            error.contains("virtual-hosted"),
            "the error must name both forms: {error}"
        );
    }

    /// The host writes every plugin field as a YAML string, so a key that only
    /// accepted a boolean could never be set by `bddkit resource add --<field>`
    /// at all. Being a string is what makes this key reachable that way.
    #[test]
    fn a_boolean_is_refused_rather_than_guessed_at() {
        let error = InstanceConfig::parse(&body(r#"{"url_style": false}"#)).expect_err("rejected");
        assert!(error.contains("url_style"), "{error}");
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
            r#"{"region": "eu-central-1", "fixtures_dir": "features/files", "url_style": "virtual-hosted"}"#,
        ))
        .expect("valid");
        assert_eq!(cfg.region, "eu-central-1");
        assert_eq!(cfg.fixtures_dir.as_deref(), Some("features/files"));
        assert_eq!(
            cfg.url_style,
            UrlStyle::VirtualHosted,
            "a real S3 endpoint needs virtual-host style"
        );
    }
}
