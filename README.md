# bddkit-s3

A `bddkit` plugin exposing S3-compatible object storage (AWS S3, MinIO, ...)
as a `bddkit` resource group named `s3`.

## Building

```bash
cargo build --release
```

The library is written to `target/release/`.

## Installing

Point a plugin lock file at the built library, in one of:

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
      # url_style: path          (default; "virtual-hosted" puts the bucket in a subdomain)
      # fixtures_dir: fixtures   (base directory for `I upload file "<path>" to "<key>"`)
```

| Key | Required | Default |
|---|---|---|
| `endpoint` | yes | — |
| `bucket` | yes | — |
| `access_key` | yes | — |
| `secret_key` | yes | — |
| `region` | no | `us-east-1` |
| `url_style` | no | `path` |
| `fixtures_dir` | no | none — required only by the `upload file` / `upload saved file` steps |

**One instance is one bucket.** If your suite touches several buckets,
declare several instances and switch between them with the built-in
`I use "<name>" s3` step, the same way `I use "<name>" api` switches API
resources.

## Checking a configuration

The plugin answers two questions about the block above, beyond the
well-formedness check every host runs at startup.

**What keys does this take?** The manifest carries the same table, so the list
can be read without opening this file:

```bash
bddkit resource fields s3 --config suite.yaml
```

**Does this configuration reach anything?** The plugin exports a live probe,
which a host runs only when asked:

```bash
bddkit doctor --config suite.yaml --live
#   ✓ plugin s3.main        probed clean
```

The probe sends one `HEAD` at the bucket root and names what actually failed,
because that text is all a report shows:

| What is wrong | What it says |
|---|---|
| nothing answers at `endpoint` | `cannot reach http://…: …` |
| the credentials are refused | `… refused this request for bucket "…": the credentials … were rejected` |
| the bucket is not there | `bucket "…" was not found at http://…` |
| `region` names the wrong one | `… redirected bucket "…" without saying where …` |

The `403` and `404` lines hedge on purpose. AWS answers `404` both for a bucket
that is not there and for one these credentials may not see — deliberately, so
that a stranger cannot probe for someone else's bucket — and it refuses a
bucket-level `HEAD` with `403` when a key carries `s3:GetObject` but not
`s3:ListBucket`, which is a key the steps themselves would work with.

The region line comes from a quirk worth knowing: AWS answers a bucket asked
for in the wrong region with `301`, carrying the real region in
`x-amz-bucket-region` and **no** `Location`. The HTTP client follows redirects,
finds nothing to follow, and reports a transport failure, so the probe never
sees the status. It recognises the case and names `region` rather than claiming
the endpoint is unreachable — which it is not.

The probe times out after five seconds rather than the client default of sixty:
an endpoint that drops packets instead of refusing them must not hold `doctor`
for two minutes.

It opens a connection, so it is never run during a suite — startup validation
stays offline and stays fast, which is what turns a typo in the config into a
failure before the first request rather than halfway through the run. The probe
creates nothing: a bucket must already exist, as it must for the steps.

bddkit reads both from 0.2.0 on. **Both are optional, and an older host ignores
both** — an unknown manifest key is skipped and an optional symbol is never
resolved. On 0.1.1 `resource fields` says the plugin describes no fields,
`doctor --live` reports the probe as not available rather than as a failure,
and everything else works unchanged.

## Steps

The plugin provides steps to upload, download, save, delete, count, and list
objects; read metadata; create presigned GET/PUT URLs; and assert object
content, size, content type, metadata, listings, and access control. See
`steps_json()` in `src/lib.rs` for the exact Gherkin patterns.

## Running the tests

Unit tests need no external services:

```bash
cargo test --lib
```

The end-to-end suite needs a `bddkit` binary and MinIO. Any release from
0.1.1 on carries the plugin ABI, so the host comes from crates.io:

```bash
cargo install bddkit --version 0.1.1 --locked
docker compose up minio-init
cargo test --test e2e
```

The binary is looked for in `$BDDKIT_BIN`, then on `PATH`, then in a sibling
`../bddkit` checkout (release before debug) — set `BDDKIT_BIN` to test against a
host you built yourself. The three tests that drive the section above through
the host need one new enough to read the config contract (0.2.0), and skip
themselves, saying so, on one that is not.

To run against an S3 you already have instead of the compose file's, point these
at it — the unit tests read the same four, and skip the three live probe cases
when they are unset:

```bash
export BDDKIT_S3_ENDPOINT=http://127.0.0.1:9000
export BDDKIT_S3_BUCKET=my-test-bucket
export BDDKIT_S3_ACCESS_KEY=…
export BDDKIT_S3_SECRET_KEY=…
```

The bucket must exist, and the suite writes objects into it.

`cargo test` runs unit tests and the end-to-end suite when a host is available.

## Known limits

1. **No bucket lifecycle steps.** A bucket belongs to a config entry, so a
   bucket must be created by your infrastructure before the test runs.
2. **`I presign … valid for "0" seconds` is legal** and yields an
   already-expired URL. A typo of `"0"` for `"60"` therefore fails the *next*
   step, confusingly, rather than the presign step itself.
3. **A missing metadata key and an empty metadata value are indistinguishable**
   — both compare equal to `""`.
4. **`I delete all objects under "<prefix>"` lists then deletes**, so it can
   race a parallel feature file writing into the same prefix. Scope data with
   `<<unique()>>` or put the files in one `@serial` chain.
5. **Text assertions reject a non-UTF-8 body** rather than comparing
   replacement characters; use `I save "<key>" as "<name>"` for binary
   objects.

## Recipes

### A presigned URL expires

Use bddkit's HTTP steps to request a presigned URL and wait for its expiry:

```gherkin
When I presign a "GET" url for "report.pdf" valid for "2" seconds as "url"
And I request "<<url>>" using HTTP GET
And I expect the next assertion to pass within "10" seconds, checking every "500" milliseconds
Then the response code is 403
```
