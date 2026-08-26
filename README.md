# bddkit-s3

A `bddkit` plugin exposing S3-compatible object storage (AWS S3, MinIO, ...)
as a `bddkit` resource group named `s3`. It targets host ABI version 1
(`bddkit_abi_version` returns `1`) and declares 22 steps across the `s3` group
— see [Steps](#steps) below for where the authoritative list lives.

The plugin is written against
[`docs/plugin-authoring.md`](https://github.com/sergeym/bddkit/blob/main/docs/plugin-authoring.md)
in the `bddkit` repository; it must never need the host's source to build or
understand.

## Building

```bash
cargo build --release
```

This produces a `cdylib`: `libbddkit_s3.so` on Linux, `libbddkit_s3.dylib` on
macOS, `bddkit_s3.dll` on Windows, under `target/release/`.

## Installing (hand-written lock file)

There is no `bddkit plugin install` yet. Point a lock file at the built
library by hand, in one of:

- `<directory of the --config file>/.bddkit/plugins.yaml` (project scope, takes precedence)
- `~/.config/bddkit/plugins.yaml` (user scope)

```yaml
# .bddkit/plugins.yaml
plugin:
  - name: s3
    path: ../../bddkit-s3/target/release/libbddkit_s3.so
```

- `name` must equal the plugin's manifest name, `s3`.
- `path` is absolute, or **relative to the lock file's own directory**
  (`.bddkit/`, not the project root, and not the current working directory).
- **`~` is not expanded** in `path` — a `~/...` path reaches `dlopen` verbatim
  and fails with a confusing "no such file".

## Configuring an instance

Declare one or more named instances under `resources.s3` in the ordinary
`bddkit` config:

```yaml
resources:
  s3:
    main:
      endpoint: http://localhost:9000
      bucket: apibdd-it
      access_key: bddkit
      secret_key: bddkit-secret
      # optional:
      # region: us-east-1        (default)
      # path_style: true         (default; set false for virtual-hosted-style addressing)
      # fixtures_dir: fixtures   (base directory for `I upload file "<path>" to "<key>"`)
```

| Key | Required | Default |
|---|---|---|
| `endpoint` | yes | — |
| `bucket` | yes | — |
| `access_key` | yes | — |
| `secret_key` | yes | — |
| `region` | no | `us-east-1` |
| `path_style` | no | `true` |
| `fixtures_dir` | no | none — required only by the `upload file` / `upload saved file` steps |

**One instance is one bucket.** If your suite touches several buckets,
declare several instances and switch between them with the built-in
`I use "<name>" s3` step, the same way `I use "<name>" api` switches API
resources.

## Steps

The authoritative step list — every pattern this plugin registers, exactly as
spelled — is `steps_json()` in `src/lib.rs`. It covers upload (docstring,
local fixture, workspace-saved file, with optional headers/metadata),
download/save, reading a single metadata field, delete (single key and whole
prefix), counting and listing under a prefix, presigning GET/PUT URLs, and
assertions for existence, content, size, content type, metadata, listing
membership, and access control (anonymous and foreign-signature refusal).

## Running the tests

**Unit tests** need nothing — no MinIO, no `bddkit`:

```bash
cargo test --lib          # 37 tests
```

**The end-to-end suite** (`tests/e2e.rs`) is the acceptance gate: the real
`bddkit` binary, this plugin loaded from a hand-written lock file, and a real
MinIO. It needs both of those present.

```bash
docker compose up minio-init                  # MinIO + the bucket; blocks until ready
BDDKIT_BIN=/path/to/bddkit cargo test --test e2e
```

The `bddkit` binary is looked for in this order: `$BDDKIT_BIN`, then `bddkit`
on `PATH`, then a sibling checkout's `../bddkit/target/debug/bddkit`. With none
of them present the suite **skips** and prints why — it does not fail, so a
clone with no host available still gets a green `cargo test`.

`cargo test` on its own runs both: the unit tests, and the end-to-end suite if
a host is reachable. Read its output rather than only its exit code — a skipped
end-to-end suite is reported as passing.

> **`cargo install bddkit` is not enough.** It installs the published release,
> and no published version of bddkit carries the plugin ABI — `src/plugin/`
> exists only on the unmerged plugin branch. Until that ships, build the host
> from a checkout of that branch and point `BDDKIT_BIN` at the result.

## Known limits

These are all real, all deliberate, and discovered during implementation —
read this before filing a bug for one of them.

1. **No bucket lifecycle steps.** A bucket belongs to a config entry, so a
   bucket that exists only at runtime could not be addressed. Create buckets
   with your infrastructure, not with a test.
2. **`I presign … valid for "0" seconds` is legal** and yields an
   already-expired URL. A typo of `"0"` for `"60"` therefore fails the *next*
   step, confusingly, rather than the presign step itself.
3. **`anonymous access to "<key>" should be denied` answers `fatal` on a
   404**, so pointing it at a key that does not exist reports "answered 404,
   expected 401 or 403" rather than a missing-object error.
4. **A missing metadata key and an empty metadata value are indistinguishable**
   — both compare equal to `""`. This is the same ceiling `bddkit` documents
   for its own `I extract` step with SQL NULL.
5. **A failed listing carries no HTTP exchange in its diagnostic.** `rust-s3`
   parses every response body as a `ListBucketResult` regardless of status; on
   a non-2xx the body is an S3 `<Error>` document, the XML parse fails, and
   the status and body are gone before the plugin ever sees them. This is a
   library ceiling, not a choice made here.
6. **`I delete all objects under "<prefix>"` lists then deletes**, so it races
   a parallel feature file writing into the same prefix — the same ceiling as
   bddkit's own `I delete all "<table>"`. Scope data with `<<unique()>>` or put
   the files in one `@serial` chain.
7. **Text assertions reject a non-UTF-8 body** rather than comparing
   replacement characters; use `I save "<key>" as "<name>"` for binary
   objects.
8. **There is no debug output yet.** The dispatch payload's `debug` flag is
   parsed but unused, so `I am in debug mode` does not make this plugin trace
   its requests.

## Recipes

### A presigned URL expires

No plugin step is needed for this — bddkit's own HTTP steps can drive a
presigned URL, and its eventual-assertion modifier can wait out the expiry:

```gherkin
When I presign a "GET" url for "report.pdf" valid for "2" seconds as "url"
And I request "<<url>>" using HTTP GET
And I expect the next assertion to pass within "10" seconds, checking every "500" milliseconds
Then the response code is 403
```

Verified against a live MinIO: the presigned GET succeeds immediately after
issue, and once past its 2-second validity the same URL answers 403, which
the eventual assertion picks up within the 10-second window.
