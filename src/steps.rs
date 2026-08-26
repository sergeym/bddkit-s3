//! One function per step. The `match` here names every index in
//! `crate::steps_json`, in the same order.

use crate::client::Instance;
use crate::files;
use crate::reply::{Ctx, Exchange, fatal, passed};

pub struct Request {
    pub args: Vec<String>,
    pub docstring: Option<String>,
    pub table: Option<Vec<Vec<String>>>,
    pub ctx: Ctx,
}

impl Request {
    pub fn parse(value: &serde_json::Value) -> Self {
        let args = value["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            args,
            docstring: value["docstring"].as_str().map(str::to_string),
            table: value["table"].as_array().map(|rows| {
                rows.iter()
                    .map(|row| {
                        row.as_array()
                            .map(|cells| {
                                cells
                                    .iter()
                                    .map(|c| c.as_str().unwrap_or_default().to_string())
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            }),
            ctx: Ctx {
                artifacts_dir: value["artifacts_dir"].as_str().unwrap_or_default().to_string(),
                workspace_dir: value["workspace_dir"].as_str().unwrap_or_default().to_string(),
                debug: value["debug"].as_bool().unwrap_or(false),
            },
        }
    }

    /// A capture the pattern guarantees. Absent means the host and this table
    /// disagree about the step, which is a bug to report, never to panic on.
    pub fn arg(&self, index: usize) -> Result<&str, String> {
        self.args
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("the step payload has no argument {index}"))
    }
}

macro_rules! todo_step {
    ($($name:ident),+ $(,)?) => {
        $(fn $name(_instance: &Instance, _request: &Request) -> Result<String, String> {
            Err(concat!(stringify!($name), " is not implemented yet").to_string())
        })+
    };
}

#[derive(Default, Debug)]
pub struct Headers {
    pub content_type: Option<String>,
    pub metadata: Vec<(String, String)>,
}

/// Row 0 is the header row — the host does not strip it, and gherkin
/// guarantees every row is as long as row 0. A row can still be empty if row
/// 0 itself is (a degenerate table), so cells are read with `.first()`/`.get()`
/// rather than indexed, to turn that into an error instead of a panic.
pub fn headers_from(table: &Option<Vec<Vec<String>>>) -> Result<Headers, String> {
    let mut headers = Headers::default();
    let Some(rows) = table else {
        return Ok(headers);
    };
    for row in rows.iter().skip(1) {
        let name = row.first().map(|s| s.trim()).unwrap_or("");
        let value = row.get(1).map(String::as_str).unwrap_or("");
        match name {
            "content-type" => headers.content_type = Some(value.to_string()),
            other if other.starts_with("meta:") => headers
                .metadata
                .push((other["meta:".len()..].to_string(), value.to_string())),
            other => {
                return Err(format!(
                    "unknown header {other:?}: the table understands \"content-type\" and \"meta:<name>\""
                ));
            }
        }
    }
    Ok(headers)
}

fn put(
    instance: &Instance,
    key: &str,
    body: &[u8],
    headers: &Headers,
    ctx: &Ctx,
) -> Result<String, String> {
    let mut request = instance.bucket.put_object_builder(key, body);
    if let Some(content_type) = &headers.content_type {
        request = request.with_content_type(content_type);
    }
    for (name, value) in &headers.metadata {
        request = request
            .with_metadata(name, value)
            .map_err(|e| format!("cannot set metadata {name:?}: {e}"))?;
    }
    let response = request
        .execute()
        .map_err(|e| format!("PUT {key} failed: {e}"))?;
    let status = response.status_code();
    if (200..300).contains(&status) {
        return Ok(passed());
    }
    // A refused write is not something waiting fixes.
    Ok(fatal(
        &format!("PUT {key} answered {status}"),
        Some(Exchange {
            title: format!("PUT {key}"),
            url: instance.bucket.url(),
            status,
            body: String::from_utf8_lossy(response.bytes()).into_owned(),
        }),
        ctx,
    ))
}

fn upload_docstring(instance: &Instance, request: &Request) -> Result<String, String> {
    let key = request.arg(0)?;
    let body = request
        .docstring
        .as_deref()
        .ok_or_else(|| "this step needs a doc string for the object body".to_string())?;
    put(instance, key, body.as_bytes(), &Headers::default(), &request.ctx)
}

fn read_local(path: &std::path::Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn upload_fixture(instance: &Instance, request: &Request) -> Result<String, String> {
    let path = files::fixture(&instance.fixtures_dir, request.arg(0)?);
    let body = read_local(&path)?;
    put(instance, request.arg(1)?, &body, &Headers::default(), &request.ctx)
}

fn upload_fixture_with_headers(instance: &Instance, request: &Request) -> Result<String, String> {
    let path = files::fixture(&instance.fixtures_dir, request.arg(0)?);
    let body = read_local(&path)?;
    let headers = headers_from(&request.table)?;
    put(instance, request.arg(1)?, &body, &headers, &request.ctx)
}

fn upload_saved(instance: &Instance, request: &Request) -> Result<String, String> {
    let path = files::in_workspace(&request.ctx.workspace_dir, request.arg(0)?)?;
    let body = read_local(&path)?;
    put(instance, request.arg(1)?, &body, &Headers::default(), &request.ctx)
}

todo_step!(
    download_to_var,
    save_to_workspace,
    read_field,
    delete_object,
    delete_prefix,
    count_prefix,
    presign,
    should_exist,
    should_not_exist,
    should_contain,
    should_equal,
    should_have_size,
    should_have_metadata,
    should_have_content_type,
    count_should_be,
    listing_should_contain,
    anonymous_should_be_denied,
    foreign_signature_should_be_rejected,
);

pub fn route(instance: &Instance, step_index: u32, request: &Request) -> String {
    let reply = match step_index {
        0 => upload_docstring(instance, request),
        1 => upload_fixture(instance, request),
        2 => upload_fixture_with_headers(instance, request),
        3 => upload_saved(instance, request),
        4 => download_to_var(instance, request),
        5 => save_to_workspace(instance, request),
        6 => read_field(instance, request),
        7 => delete_object(instance, request),
        8 => delete_prefix(instance, request),
        9 => count_prefix(instance, request),
        10 => presign(instance, request),
        11 => should_exist(instance, request),
        12 => should_not_exist(instance, request),
        13 => should_contain(instance, request),
        14 => should_equal(instance, request),
        15 => should_have_size(instance, request),
        16 => should_have_content_type(instance, request),
        17 => should_have_metadata(instance, request),
        18 => count_should_be(instance, request),
        19 => listing_should_contain(instance, request),
        20 => anonymous_should_be_denied(instance, request),
        21 => foreign_signature_should_be_rejected(instance, request),
        other => Err(format!("unknown step {other}")),
    };
    // A step function answers `Err` for anything it could not even attempt: a
    // bad argument, an unreadable local file, a transport failure. Everything
    // it *did* observe it reports itself, because only it knows whether the
    // observation means `not_yet` or `fatal`.
    reply.unwrap_or_else(|error| fatal(&error, None, &request.ctx))
}

#[cfg(test)]
pub fn route_for_test(step_index: u32, args: &[String]) -> String {
    let config = crate::config::InstanceConfig::parse(&serde_json::json!({
        "endpoint": "http://127.0.0.1:1",
        "bucket": "unit-test",
        "access_key": "k",
        "secret_key": "s"
    }))
    .expect("valid");
    let instance = Instance::connect(&config).expect("built");
    let request = Request {
        args: args.to_vec(),
        docstring: None,
        table: None,
        ctx: Ctx {
            artifacts_dir: std::env::temp_dir().display().to_string(),
            workspace_dir: std::env::temp_dir().display().to_string(),
            debug: false,
        },
    };
    route(&instance, step_index, &request)
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_unknown_step_index_is_fatal_and_names_the_index() {
        let raw = crate::steps::route_for_test(99, &[]);
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(v["status"], "fatal");
        assert!(v["error"].as_str().expect("error").contains("99"));
    }

    #[test]
    fn a_missing_argument_is_fatal_rather_than_a_panic() {
        // Step 4 is `I download "<key>" as "<var>"`: two captures. A payload
        // with one is a host or ABI bug, and it must surface as a reply.
        let raw = crate::steps::route_for_test(4, &["only-one".to_string()]);
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(v["status"], "fatal");
    }

    #[test]
    fn an_upload_of_a_missing_fixture_is_fatal_and_names_the_path() {
        // Step 1 is `I upload file "<path>" to "<key>"`.
        let raw = crate::steps::route_for_test(
            1,
            &["no-such-file.pdf".to_string(), "report.pdf".to_string()],
        );
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(v["status"], "fatal", "a missing local file cannot be waited for");
        assert!(
            v["error"].as_str().expect("error").contains("no-such-file.pdf"),
            "{}",
            v["error"]
        );
    }

    #[test]
    fn a_header_table_maps_content_type_and_metadata() {
        let table = vec![
            vec!["name".to_string(), "value".to_string()],
            vec!["content-type".to_string(), "application/pdf".to_string()],
            vec!["meta:owner".to_string(), "alice".to_string()],
        ];
        let headers = super::headers_from(&Some(table)).expect("parsed");
        assert_eq!(headers.content_type.as_deref(), Some("application/pdf"));
        assert_eq!(headers.metadata, vec![("owner".to_string(), "alice".to_string())]);
    }

    #[test]
    fn a_header_table_rejects_a_name_it_does_not_understand() {
        let table = vec![
            vec!["name".to_string(), "value".to_string()],
            vec!["cache-control".to_string(), "no-store".to_string()],
        ];
        let error = super::headers_from(&Some(table)).expect_err("refused");
        assert!(error.contains("cache-control"), "{error}");
    }
}
