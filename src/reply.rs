//! Every reply the plugin sends is built here, so the redaction below cannot
//! be bypassed by a new call site.

/// A body larger than this goes to a file. The number is a judgement, not a
/// limit anything enforces: big enough for an S3 error document, small enough
/// that a failure dump stays readable in a terminal.
pub const MAX_INLINE_BODY: usize = 8 * 1024;

// No caller until Task 9 wires dispatch into steps.rs.
#[allow(dead_code)]
pub struct Ctx {
    pub artifacts_dir: String,
    pub workspace_dir: String,
    pub debug: bool,
}

// No caller until Task 9 wires dispatch into steps.rs.
#[allow(dead_code)]
pub struct Exchange {
    pub title: String,
    pub url: String,
    pub status: u16,
    pub body: String,
}

/// A presigned URL is a bearer credential: whoever reads the signature can use
/// it until it expires, and a failure dump reaches CI logs. Everything else in
/// the URL stays, because that is what makes the dump useful.
fn redact(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let scrubbed: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) if key.eq_ignore_ascii_case("X-Amz-Signature") => {
                format!("{key}=REDACTED")
            }
            _ => pair.to_string(),
        })
        .collect();
    format!("{base}?{}", scrubbed.join("&"))
}

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
fn truncate_at_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn diagnostics_of(exchange: Option<Exchange>, ctx: &Ctx) -> serde_json::Value {
    let Some(exchange) = exchange else {
        return serde_json::json!([]);
    };
    let head = format!("{}\nstatus: {}", redact(&exchange.url), exchange.status);
    if exchange.body.len() <= MAX_INLINE_BODY {
        return serde_json::json!([{
            "title": exchange.title,
            "kind": "http",
            "content": format!("{head}\n\n{}", exchange.body),
            "path": null
        }]);
    }
    // The host allocates `artifacts_dir` but does not create it.
    let path = std::path::Path::new(&ctx.artifacts_dir).join("response.txt");
    let written = std::fs::create_dir_all(&ctx.artifacts_dir)
        .and_then(|()| std::fs::write(&path, &exchange.body))
        .is_ok();
    if written {
        // The body lives in the file; the URL and status stay inline, because
        // a reader needs them to know what failed before opening anything.
        serde_json::json!([{
            "title": exchange.title,
            "kind": "http",
            "content": head,
            "path": path.display().to_string()
        }])
    } else {
        // Losing the evidence entirely is worse than a truncated dump.
        let truncated = truncate_at_char_boundary(&exchange.body, MAX_INLINE_BODY);
        serde_json::json!([{
            "title": exchange.title,
            "kind": "http",
            "content": format!("{head}\n\n{truncated}\n… truncated"),
            "path": null
        }])
    }
}

// No caller until Task 9 wires dispatch into steps.rs.
#[allow(dead_code)]
pub fn passed() -> String {
    r#"{"status":"passed"}"#.to_string()
}

// No caller until Task 9 wires dispatch into steps.rs.
#[allow(dead_code)]
pub fn passed_with(vars: serde_json::Value) -> String {
    serde_json::json!({"status": "passed", "vars": vars}).to_string()
}

/// One fresh observation says the condition is not met yet. Only an assertion
/// may answer this, and only an armed eventual assertion gives it a second
/// attempt — without one it is simply a failure, which is why the message must
/// say what was observed.
// No caller until Task 9 wires dispatch into steps.rs.
#[allow(dead_code)]
pub fn not_yet(error: &str, exchange: Option<Exchange>, ctx: &Ctx) -> String {
    serde_json::json!({
        "status": "not_yet",
        "error": error,
        "diagnostics": diagnostics_of(exchange, ctx)
    })
    .to_string()
}

/// The observation itself failed; retrying cannot help.
// No caller until Task 9 wires dispatch into steps.rs.
#[allow(dead_code)]
pub fn fatal(error: &str, exchange: Option<Exchange>, ctx: &Ctx) -> String {
    serde_json::json!({
        "status": "fatal",
        "error": error,
        "diagnostics": diagnostics_of(exchange, ctx)
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &str) -> Ctx {
        Ctx {
            artifacts_dir: dir.to_string(),
            workspace_dir: dir.to_string(),
            debug: false,
        }
    }

    #[test]
    fn a_passed_reply_carries_vars_and_no_diagnostics() {
        let raw = passed_with(serde_json::json!({"etag": "abc"}));
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(v["status"], "passed");
        assert_eq!(v["vars"]["etag"], "abc");
    }

    #[test]
    fn a_failure_renders_the_exchange_as_a_diagnostic() {
        let ex = Exchange {
            title: "GET /acme/report.pdf".into(),
            url: "http://localhost:9000/acme/report.pdf".into(),
            status: 404,
            body: "<Error>no such key</Error>".into(),
        };
        let raw = fatal("the object is missing", Some(ex), &ctx("/tmp/nonexistent-dir"));
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(v["status"], "fatal");
        assert_eq!(v["error"], "the object is missing");
        assert_eq!(v["diagnostics"][0]["kind"], "http");
        let content = v["diagnostics"][0]["content"].as_str().expect("content");
        assert!(content.contains("404"), "{content}");
        assert!(content.contains("no such key"), "{content}");
    }

    #[test]
    fn a_signature_is_never_written_into_a_diagnostic() {
        let url = "http://localhost:9000/acme/report.pdf\
                   ?X-Amz-Credential=AKIA%2F20260827&X-Amz-Signature=deadbeefcafe&X-Amz-Expires=60";
        let ex = Exchange {
            title: "GET presigned".into(),
            url: url.into(),
            status: 403,
            body: String::new(),
        };
        let raw = not_yet("still refused", Some(ex), &ctx("/tmp/nonexistent-dir"));
        assert!(
            !raw.contains("deadbeefcafe"),
            "a presigned URL is a bearer credential and failure dumps reach CI logs: {raw}"
        );
        assert!(raw.contains("X-Amz-Signature=REDACTED"), "{raw}");
        assert!(
            raw.contains("X-Amz-Expires=60"),
            "everything that is not the signature stays readable: {raw}"
        );
    }

    #[test]
    fn redaction_holds_for_every_shape_a_signed_url_takes() {
        // Written as a table because the failure this guards against is a
        // signature surviving in ONE arrangement, not in all of them.
        let cases = [
            ("http://h/k", "http://h/k", "no query at all"),
            (
                "http://h/k?X-Amz-Signature=abc",
                "http://h/k?X-Amz-Signature=REDACTED",
                "the only parameter",
            ),
            (
                "http://h/k?a=1&X-Amz-Signature=abc&b=2",
                "http://h/k?a=1&X-Amz-Signature=REDACTED&b=2",
                "in the middle",
            ),
            (
                "http://h/k?a=1&X-Amz-Signature=abc",
                "http://h/k?a=1&X-Amz-Signature=REDACTED",
                "last",
            ),
            (
                "http://h/k?x-amz-signature=abc",
                "http://h/k?x-amz-signature=REDACTED",
                "lowercase, as some clients emit it",
            ),
            (
                "http://h/k?X-Amz-Signature=a&X-Amz-Signature=b",
                "http://h/k?X-Amz-Signature=REDACTED&X-Amz-Signature=REDACTED",
                "repeated",
            ),
            (
                "http://h/k?Not-X-Amz-Signature=keep",
                "http://h/k?Not-X-Amz-Signature=keep",
                "a name merely containing it must survive",
            ),
            (
                "http://h/k?X-Amz-Credential=AKIA%2F20260827&X-Amz-Expires=60",
                "http://h/k?X-Amz-Credential=AKIA%2F20260827&X-Amz-Expires=60",
                "nothing else is touched",
            ),
        ];
        for (input, expected, why) in cases {
            assert_eq!(redact(input), expected, "{why}");
        }
    }

    #[test]
    fn an_oversized_body_is_spilled_to_a_file_instead_of_inlined() {
        let dir = std::env::temp_dir().join(format!("bddkit-s3-spill-{}", std::process::id()));
        let ex = Exchange {
            title: "GET big".into(),
            url: "http://localhost:9000/acme/big".into(),
            status: 500,
            body: "x".repeat(MAX_INLINE_BODY + 1),
        };
        let raw = fatal("boom", Some(ex), &ctx(&dir.display().to_string()));
        let v: serde_json::Value = serde_json::from_str(&raw).expect("JSON");
        let path = v["diagnostics"][0]["path"].as_str().expect("path");
        assert!(
            std::path::Path::new(path).exists(),
            "the plugin creates artifacts_dir itself; the host does not"
        );
        let content = v["diagnostics"][0]["content"].as_str().expect("content");
        assert!(
            !content.contains(&"x".repeat(MAX_INLINE_BODY)),
            "a megabyte of body must not be inlined into the dump"
        );
        assert!(
            content.contains("500") && content.contains("acme/big"),
            "the url and status stay in the dump; only the body moves to the file: {content}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
