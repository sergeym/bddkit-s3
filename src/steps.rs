//! One function per step. The `match` here names every index in
//! `crate::steps_json`, in the same order.

use crate::client::Instance;
use crate::files;
use crate::reply::{Ctx, Exchange, fatal, not_yet, passed, passed_with};

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
/// rather than indexed. A missing cell reads as an empty string, which then
/// fails the name check below — an error, never a panic.
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

/// The one place a failed request becomes evidence. The URL names the object,
/// not just the bucket: a dump listing eight failures against one bucket is
/// unreadable if every line shows the same URL.
fn exchange_for(instance: &Instance, verb: &str, key: &str, status: u16, body: &[u8]) -> Exchange {
    Exchange {
        title: format!("{verb} {key}"),
        url: format!("{}/{key}", instance.bucket.url().trim_end_matches('/')),
        status,
        body: String::from_utf8_lossy(body).into_owned(),
    }
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
        Some(exchange_for(instance, "PUT", key, status, response.bytes())),
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

/// One GET, with the status handed back rather than turned into an error:
/// only the caller knows whether a 404 means `not_yet` or `fatal`.
fn get(instance: &Instance, key: &str) -> Result<(u16, Vec<u8>), String> {
    let response = instance
        .bucket
        .get_object(key)
        .map_err(|e| format!("GET {key} failed: {e}"))?;
    Ok((response.status_code(), response.bytes().to_vec()))
}

fn download_to_var(instance: &Instance, request: &Request) -> Result<String, String> {
    let (key, name) = (request.arg(0)?, request.arg(1)?);
    let (status, body) = get(instance, key)?;
    if !(200..300).contains(&status) {
        return Ok(fatal(
            &format!("GET {key} answered {status}"),
            Some(exchange_for(instance, "GET", key, status, &body)),
            &request.ctx,
        ));
    }
    let text = String::from_utf8(body)
        .map_err(|_| format!("{key} is not UTF-8; use `I save \"{key}\" as \"…\"` instead"))?;
    Ok(passed_with(serde_json::json!({ name: text })))
}

fn save_to_workspace(instance: &Instance, request: &Request) -> Result<String, String> {
    let key = request.arg(0)?;
    // Checked before the request: a bad name is the tester's mistake, and
    // finding out after a download wastes a round trip and reads worse.
    let path = files::in_workspace(&request.ctx.workspace_dir, request.arg(1)?)?;
    let (status, body) = get(instance, key)?;
    if !(200..300).contains(&status) {
        return Ok(fatal(
            &format!("GET {key} answered {status}"),
            Some(exchange_for(instance, "GET", key, status, &body)),
            &request.ctx,
        ));
    }
    std::fs::write(&path, &body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(passed())
}

fn read_field(instance: &Instance, request: &Request) -> Result<String, String> {
    let (field, key, name) = (request.arg(0)?, request.arg(1)?, request.arg(2)?);
    // Checked before the request: an unknown field name is the tester's
    // mistake, not something S3 can answer, and a HEAD round trip would only
    // delay the same error.
    if field != "etag"
        && field != "size"
        && field != "content-type"
        && !field.starts_with("meta:")
    {
        return Err(format!(
            "unknown field {field:?}: known fields are \"etag\", \"size\", \"content-type\" and \"meta:<name>\""
        ));
    }
    let (head, status) = instance
        .bucket
        .head_object(key)
        .map_err(|e| format!("HEAD {key} failed: {e}"))?;
    if !(200..300).contains(&status) {
        return Ok(fatal(
            &format!("HEAD {key} answered {status}"),
            Some(exchange_for(instance, "HEAD", key, status, &[])),
            &request.ctx,
        ));
    }
    let value = match field {
        "etag" => head.e_tag.clone().unwrap_or_default(),
        "size" => head.content_length.unwrap_or_default().to_string(),
        "content-type" => head.content_type.clone().unwrap_or_default(),
        other => head
            .metadata
            .as_ref()
            .and_then(|m| m.get(&other["meta:".len()..]).cloned())
            .unwrap_or_default(),
    };
    Ok(passed_with(serde_json::json!({ name: value })))
}

fn delete_object(instance: &Instance, request: &Request) -> Result<String, String> {
    let key = request.arg(0)?;
    let response = instance
        .bucket
        .delete_object(key)
        .map_err(|e| format!("DELETE {key} failed: {e}"))?;
    let status = response.status_code();
    // S3 deletes are idempotent: 204 for a key that was there and one that
    // never was. Only a refusal is a failure.
    if (200..300).contains(&status) || status == 404 {
        return Ok(passed());
    }
    Ok(fatal(
        &format!("DELETE {key} answered {status}"),
        Some(exchange_for(instance, "DELETE", key, status, response.bytes())),
        &request.ctx,
    ))
}

/// A HEAD reduced to what an assertion needs. `Err` is reserved for a
/// transport failure, which no amount of waiting repairs.
fn head(
    instance: &Instance,
    key: &str,
) -> Result<(u16, s3::serde_types::HeadObjectResult), String> {
    let (result, status) = instance
        .bucket
        .head_object(key)
        .map_err(|e| format!("HEAD {key} failed: {e}"))?;
    Ok((status, result))
}

fn should_exist(instance: &Instance, request: &Request) -> Result<String, String> {
    let key = request.arg(0)?;
    let (status, _) = head(instance, key)?;
    if (200..300).contains(&status) {
        return Ok(passed());
    }
    if status == 404 {
        // The observation succeeded and said "not there, just now". An armed
        // eventual assertion gets another attempt; without one this is a plain
        // failure, which is why the message says what was seen.
        return Ok(not_yet(&format!("{key} is not in the bucket yet"), None, &request.ctx));
    }
    Ok(fatal(
        &format!("HEAD {key} answered {status}"),
        Some(exchange_for(instance, "HEAD", key, status, &[])),
        &request.ctx,
    ))
}

fn should_not_exist(instance: &Instance, request: &Request) -> Result<String, String> {
    let key = request.arg(0)?;
    let (status, _) = head(instance, key)?;
    if status == 404 {
        return Ok(passed());
    }
    if (200..300).contains(&status) {
        return Ok(not_yet(&format!("{key} is still in the bucket"), None, &request.ctx));
    }
    Ok(fatal(
        &format!("HEAD {key} answered {status}"),
        Some(exchange_for(instance, "HEAD", key, status, &[])),
        &request.ctx,
    ))
}

/// The body of an object for an assertion to inspect, or the reply to hand
/// straight back because there is nothing to compare yet. Named variants
/// instead of a nested `Result<Result<..>>` — same three outcomes, easier to
/// match on.
enum ObjectBody {
    Present(String),
    Reply(String),
}

fn body_for_assertion(instance: &Instance, key: &str, ctx: &Ctx) -> Result<ObjectBody, String> {
    let (status, bytes) = get(instance, key)?;
    if (200..300).contains(&status) {
        // A binary object read as text compares nonsense either way; no
        // amount of waiting turns it into UTF-8, so this is a hard failure
        // rather than `not_yet` (same call `download_to_var` already makes).
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{key} is not UTF-8; a text assertion cannot compare it"))?;
        return Ok(ObjectBody::Present(text));
    }
    if status == 404 {
        return Ok(ObjectBody::Reply(not_yet(
            &format!("{key} is not in the bucket yet"),
            None,
            ctx,
        )));
    }
    Ok(ObjectBody::Reply(fatal(
        &format!("GET {key} answered {status}"),
        Some(exchange_for(instance, "GET", key, status, &bytes)),
        ctx,
    )))
}

fn should_contain(instance: &Instance, request: &Request) -> Result<String, String> {
    let (key, needle) = (request.arg(0)?, request.arg(1)?);
    let body = match body_for_assertion(instance, key, &request.ctx)? {
        ObjectBody::Present(body) => body,
        ObjectBody::Reply(reply) => return Ok(reply),
    };
    if body.contains(needle) {
        return Ok(passed());
    }
    Ok(not_yet(&format!("{key} does not contain {needle:?}"), None, &request.ctx))
}

fn should_equal(instance: &Instance, request: &Request) -> Result<String, String> {
    let key = request.arg(0)?;
    let expected = request
        .docstring
        .as_deref()
        .ok_or_else(|| "this step needs a doc string with the expected body".to_string())?;
    let body = match body_for_assertion(instance, key, &request.ctx)? {
        ObjectBody::Present(body) => body,
        ObjectBody::Reply(reply) => return Ok(reply),
    };
    if body == expected {
        return Ok(passed());
    }
    Ok(not_yet(&format!("{key} is {body:?}, expected {expected:?}"), None, &request.ctx))
}

/// The two string-valued HEAD-field assertions (content-type, metadata) share
/// this body so they cannot drift apart; `should_have_size` compares numbers
/// instead and is written separately below.
fn head_field_should_be(
    instance: &Instance,
    request: &Request,
    key: &str,
    label: &str,
    expected: &str,
    actual: impl Fn(&s3::serde_types::HeadObjectResult) -> Option<String>,
) -> Result<String, String> {
    let (status, result) = head(instance, key)?;
    if status == 404 {
        return Ok(not_yet(&format!("{key} is not in the bucket yet"), None, &request.ctx));
    }
    if !(200..300).contains(&status) {
        return Ok(fatal(
            &format!("HEAD {key} answered {status}"),
            Some(exchange_for(instance, "HEAD", key, status, &[])),
            &request.ctx,
        ));
    }
    let found = actual(&result).unwrap_or_default();
    if found == expected {
        return Ok(passed());
    }
    Ok(not_yet(
        &format!("{key} has {label} {found:?}, expected {expected:?}"),
        None,
        &request.ctx,
    ))
}

fn should_have_size(instance: &Instance, request: &Request) -> Result<String, String> {
    let (key, expected) = (request.arg(0)?, request.arg(1)?);
    let expected: i64 = expected
        .parse()
        .map_err(|_| format!("{expected:?} is not a whole number of bytes"))?;
    let (status, result) = head(instance, key)?;
    if status == 404 {
        return Ok(not_yet(&format!("{key} is not in the bucket yet"), None, &request.ctx));
    }
    if !(200..300).contains(&status) {
        return Ok(fatal(
            &format!("HEAD {key} answered {status}"),
            Some(exchange_for(instance, "HEAD", key, status, &[])),
            &request.ctx,
        ));
    }
    let found = result.content_length.unwrap_or_default();
    if found == expected {
        return Ok(passed());
    }
    Ok(not_yet(
        &format!("{key} has size {found}, expected {expected}"),
        None,
        &request.ctx,
    ))
}

fn should_have_content_type(instance: &Instance, request: &Request) -> Result<String, String> {
    let (key, expected) = (request.arg(0)?, request.arg(1)?);
    head_field_should_be(instance, request, key, "content type", expected, |r| {
        r.content_type.clone()
    })
}

fn should_have_metadata(instance: &Instance, request: &Request) -> Result<String, String> {
    let (key, name, expected) = (request.arg(0)?, request.arg(1)?, request.arg(2)?);
    let label = format!("metadata {name:?}");
    head_field_should_be(instance, request, key, &label, expected, |r| {
        r.metadata.as_ref().and_then(|m| m.get(name).cloned())
    })
}

todo_step!(
    delete_prefix,
    count_prefix,
    presign,
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

    #[test]
    fn saving_under_a_traversing_name_is_refused_before_any_request() {
        // Step 5 is `I save "<key>" as "<name>"`.
        let raw = crate::steps::route_for_test(
            5,
            &["report.pdf".to_string(), "../escape.pdf".to_string()],
        );
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(v["status"], "fatal");
        assert!(v["error"].as_str().expect("error").contains("escape.pdf"));
    }

    #[test]
    fn an_assertion_that_cannot_reach_the_server_is_fatal_not_not_yet() {
        // route_for_test points at 127.0.0.1:1, which refuses instantly.
        // Step 11 is `object "<key>" should exist`.
        let raw = crate::steps::route_for_test(11, &["report.pdf".to_string()]);
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(
            v["status"], "fatal",
            "a refused connection is not something polling can fix; \
             answering not_yet here would burn the tester's whole timeout"
        );
        assert!(
            !v["error"].as_str().expect("error").contains("not implemented"),
            "the step must actually run"
        );
    }

    #[test]
    fn an_unknown_read_field_is_named_in_the_error() {
        // Step 6 is `I read "<field>" of "<key>" as "<var>"`.
        let raw = crate::steps::route_for_test(
            6,
            &["colour".to_string(), "report.pdf".to_string(), "v".to_string()],
        );
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(v["status"], "fatal");
        assert!(v["error"].as_str().expect("error").contains("colour"));
    }
}
