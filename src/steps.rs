//! One function per step. The `match` here names every index in
//! `crate::steps_json`, in the same order.

use crate::client::Instance;
use crate::reply::{Ctx, fatal};

// No caller until Tasks 10-14 implement the step bodies and start reading
// captures, docstrings, and tables.
#[allow(dead_code)]
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
    // No caller until Tasks 10-14 implement the step bodies.
    #[allow(dead_code)]
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

todo_step!(
    upload_docstring,
    upload_fixture,
    upload_fixture_with_headers,
    upload_saved,
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
}
