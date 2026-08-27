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

The plugin provides steps to upload, download, save, delete, count, and list
objects; read metadata; create presigned GET/PUT URLs; and assert object
content, size, content type, metadata, listings, and access control. See
`steps_json()` in `src/lib.rs` for the exact Gherkin patterns.

## Running the tests

Unit tests need no external services:

```bash
cargo test --lib
```

The end-to-end suite needs a `bddkit` binary and MinIO:

```bash
docker compose up minio-init
BDDKIT_BIN=/path/to/bddkit cargo test --test e2e
```

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
