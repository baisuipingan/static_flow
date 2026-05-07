# llm-access Usage Journal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `llm-access` usage analytics so the API process writes a compact local journal and a separate worker consumes that journal into the existing tiered DuckDB store.

**Architecture:** Add a new `llm-usage-journal` library crate for the binary journal format, writer, reader, retention, CLI, and consumer state. Keep SQLite rollups in the API process, move DuckDB writes and settled usage queries to `llm-access-usage-worker`, and let the API process proxy legacy usage routes to the worker query port. Add an admin frontend status panel for backlog and worker progress only.

**Tech Stack:** Rust, Tokio, Axum, serde/postcard, zstd, crc32c, SQLite via `rusqlite`, DuckDB via `llm-access-store`, Yew frontend.

---

## Scope And File Map

Create:
- `llm-usage-journal/Cargo.toml`
- `llm-usage-journal/src/lib.rs`
- `llm-usage-journal/src/config.rs`
- `llm-usage-journal/src/wire.rs`
- `llm-usage-journal/src/writer.rs`
- `llm-usage-journal/src/reader.rs`
- `llm-usage-journal/src/retention.rs`
- `llm-usage-journal/src/state.rs`
- `llm-usage-journal/src/status.rs`
- `llm-usage-journal/src/cli.rs`
- `llm-usage-journal/src/bin/llm-usage-journal.rs`
- `llm-access/src/usage_journal.rs`
- `llm-access/src/usage_query.rs`
- `llm-access/src/usage_worker.rs`
- `llm-access/src/bin/llm-access-usage-worker.rs`

Modify:
- `Cargo.toml`
- `llm-access/Cargo.toml`
- `llm-access-core/src/store.rs`
- `llm-access-migrations/src/lib.rs`
- `llm-access-migrations/migrations/sqlite/0007_usage_journal_runtime_settings.sql`
- `llm-access-store/src/sqlite.rs`
- `llm-access-store/src/repository.rs`
- `llm-access-store/src/duckdb.rs`
- `llm-access/src/config.rs`
- `llm-access/src/runtime.rs`
- `llm-access/src/lib.rs`
- `llm-access/src/admin.rs`
- `frontend/src/api.rs`
- `frontend/src/pages/admin_llm_gateway.rs`

Verification command prefix for every Rust task:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
pgrep -af 'cargo|rustc|trunk|ld|lld|mold' || true
```

Run only one Rust build/check at a time.

### Task 1: Journal Crate Skeleton And Wire Format

**Files:**
- Modify: `Cargo.toml`
- Create: `llm-usage-journal/Cargo.toml`
- Create: `llm-usage-journal/src/lib.rs`
- Create: `llm-usage-journal/src/config.rs`
- Create: `llm-usage-journal/src/wire.rs`
- Test: `llm-usage-journal/src/wire.rs`

- [ ] **Step 1: Add the crate to the workspace**

Add `"llm-usage-journal"` to `workspace.members` in `Cargo.toml`.

Create `llm-usage-journal/Cargo.toml` with:

```toml
[package]
name = "llm-usage-journal"
version = "0.1.0"
edition = "2021"
publish = false

[lints]
workspace = true

[dependencies]
anyhow = { workspace = true }
crc32c = "0.6"
llm-access-core = { path = "../llm-access-core" }
postcard = { version = "1.1", features = ["alloc"] }
rusqlite = { version = "0.37", features = ["bundled"] }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
zstd = "0.13"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Define public module exports**

Create `llm-usage-journal/src/lib.rs`:

```rust
//! Local binary journal for llm-access usage diagnostics.

pub mod config;
pub mod reader;
pub mod retention;
pub mod state;
pub mod status;
pub mod wire;
pub mod writer;

pub use config::JournalConfig;
pub use reader::JournalReader;
pub use status::{JournalStatusSnapshot, WorkerProgressSnapshot};
pub use wire::{JournalUsageBatchV1, JournalUsageEventV1};
pub use writer::JournalWriter;
```

- [ ] **Step 3: Define the config object**

Create `llm-usage-journal/src/config.rs`:

```rust
//! Journal configuration.

use std::path::PathBuf;

/// Runtime settings for usage journal writing and retention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalConfig {
    /// Root directory that contains active, sealed, consuming, and bad files.
    pub root_dir: PathBuf,
    /// Maximum compressed file size before sealing.
    pub max_file_bytes: u64,
    /// Maximum file age before sealing.
    pub max_file_age_ms: u64,
    /// Maximum sealed plus stale-consuming files retained.
    pub max_files: usize,
    /// Target uncompressed block payload bytes.
    pub block_target_uncompressed_bytes: usize,
    /// Maximum events per block.
    pub block_max_events: usize,
    /// Fsync interval in milliseconds; zero means every flushed block.
    pub fsync_interval_ms: u64,
    /// zstd compression level.
    pub zstd_level: i32,
    /// Claimed-file lease age before recovery.
    pub consumer_lease_ms: u64,
    /// Whether corrupt files are deleted instead of quarantined.
    pub delete_bad_files: bool,
}

impl JournalConfig {
    /// Build production defaults for a root directory.
    pub fn new(root_dir: PathBuf) -> Self {
        Self {
            root_dir,
            max_file_bytes: 64 * 1024 * 1024,
            max_file_age_ms: 300_000,
            max_files: 128,
            block_target_uncompressed_bytes: 1024 * 1024,
            block_max_events: 1024,
            fsync_interval_ms: 250,
            zstd_level: 3,
            consumer_lease_ms: 300_000,
            delete_bad_files: false,
        }
    }
}
```

- [ ] **Step 4: Define stable wire structs and conversion tests**

Create `llm-usage-journal/src/wire.rs` with `JournalUsageEventV1`, `JournalUsageBatchV1`, `FileHeaderV1`, `BlockHeaderV1`, `FileFooterV1`, and explicit conversions from `llm_access_core::usage::UsageEvent`.

Add tests named:

```rust
#[test]
fn usage_event_converts_to_versioned_journal_event() {
    let event = test_usage_event("evt-wire-1");
    let journal = JournalUsageEventV1::from_usage_event(&event);
    assert_eq!(journal.event_id, "evt-wire-1");
    assert_eq!(journal.full_request_json.as_deref(), Some("{\"model\":\"m\"}"));
    assert_eq!(journal.schema_version, 1);
}

#[test]
fn journal_batch_round_trips_through_postcard() {
    let event = JournalUsageEventV1::from_usage_event(&test_usage_event("evt-wire-2"));
    let batch = JournalUsageBatchV1 { events: vec![event] };
    let bytes = postcard::to_allocvec(&batch).expect("encode batch");
    let decoded: JournalUsageBatchV1 = postcard::from_bytes(&bytes).expect("decode batch");
    assert_eq!(decoded.events[0].event_id, "evt-wire-2");
}
```

The helper `test_usage_event` must build a complete `UsageEvent` with `full_request_json = Some("{\"model\":\"m\"}".to_string())`.

- [ ] **Step 5: Verify Task 1**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-usage-journal wire --jobs 4
```

Expected: both wire tests pass.

- [ ] **Step 6: Commit Task 1**

```bash
git add Cargo.toml llm-usage-journal
git commit -m "feat(llm-access): add usage journal wire crate"
```

### Task 2: Journal Writer, Reader, Retention, And CLI

**Files:**
- Create: `llm-usage-journal/src/writer.rs`
- Create: `llm-usage-journal/src/reader.rs`
- Create: `llm-usage-journal/src/retention.rs`
- Create: `llm-usage-journal/src/cli.rs`
- Create: `llm-usage-journal/src/bin/llm-usage-journal.rs`
- Modify: `llm-usage-journal/src/lib.rs`

- [ ] **Step 1: Add writer and reader tests first**

Add tests:

```rust
#[test]
fn writer_seals_file_with_valid_footer_and_reader_reads_batch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = JournalConfig::new(dir.path().to_path_buf());
    let mut writer = JournalWriter::open(config).expect("open writer");
    writer.append_events(&[test_usage_event("evt-journal-1")]).expect("append");
    let sealed = writer.seal_current_file().expect("seal");
    let batches = JournalReader::open(&sealed).expect("open reader").read_all_batches().expect("read");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].events[0].event_id, "evt-journal-1");
}

#[test]
fn reader_rejects_corrupted_block_crc() {
    let path = write_one_event_journal("evt-corrupt");
    corrupt_one_payload_byte(&path);
    let err = JournalReader::open(&path).and_then(|reader| reader.read_all_batches()).expect_err("crc must fail");
    assert!(err.to_string().contains("crc"));
}

#[test]
fn retention_deletes_oldest_sealed_file_but_keeps_active_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = JournalConfig { max_files: 1, ..JournalConfig::new(dir.path().to_path_buf()) };
    create_sealed_file(dir.path(), 1);
    create_sealed_file(dir.path(), 2);
    create_active_file(dir.path(), 3);
    let report = llm_usage_journal::retention::enforce_retention(&config).expect("retention");
    assert_eq!(report.deleted_files, 1);
    assert!(dir.path().join("active/usage-000000000003.open").exists());
}
```

- [ ] **Step 2: Implement writer and reader**

Implement block files with:

- magic `LLMUJNL1`
- `postcard` batch encoding
- `zstd::stream::encode_all`
- `crc32c::crc32c`
- active path `active/usage-<sequence>.open`
- sealed path `sealed/usage-<sequence>.journal`

`JournalWriter::append_events` must buffer up to `block_max_events` or `block_target_uncompressed_bytes` before flushing. `seal_current_file` must flush pending events, write a footer, sync, and rename atomically into `sealed/`.

- [ ] **Step 3: Implement CLI commands**

`llm-usage-journal list --dir <root>` prints one line per file with state, sequence, bytes, and event count.

`llm-usage-journal inspect <file>` validates header, CRCs, and footer.

`llm-usage-journal dump <file> --limit 50` prints JSON lines.

`llm-usage-journal grep --dir <root> --key-name <name> --since <duration>` scans sealed and active files and prints matching JSON lines.

- [ ] **Step 4: Verify Task 2**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-usage-journal --jobs 4
cargo run -p llm-usage-journal --bin llm-usage-journal -- list --dir /tmp/llm-usage-journal-smoke
```

Expected: tests pass; CLI returns a valid empty-list response for a new directory.

- [ ] **Step 5: Commit Task 2**

```bash
git add llm-usage-journal
git commit -m "feat(llm-access): implement usage journal files"
```

### Task 3: Runtime Config And Status Contracts

**Files:**
- Modify: `llm-access-core/src/store.rs`
- Create: `llm-access-migrations/migrations/sqlite/0007_usage_journal_runtime_settings.sql`
- Modify: `llm-access-migrations/src/lib.rs`
- Modify: `llm-access-store/src/sqlite.rs`
- Modify: `llm-access-store/src/repository.rs`
- Modify: `frontend/src/api.rs`

- [ ] **Step 1: Add config fields and defaults**

Add defaults to `llm-access-core/src/store.rs`:

```rust
pub const DEFAULT_USAGE_JOURNAL_ENABLED: bool = true;
pub const DEFAULT_USAGE_JOURNAL_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_USAGE_JOURNAL_MAX_FILE_AGE_MS: u64 = 300_000;
pub const DEFAULT_USAGE_JOURNAL_MAX_FILES: u64 = 128;
pub const DEFAULT_USAGE_JOURNAL_BLOCK_TARGET_UNCOMPRESSED_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_USAGE_JOURNAL_BLOCK_MAX_EVENTS: u64 = 1024;
pub const DEFAULT_USAGE_JOURNAL_FSYNC_INTERVAL_MS: u64 = 250;
pub const DEFAULT_USAGE_JOURNAL_ZSTD_LEVEL: i64 = 3;
pub const DEFAULT_USAGE_JOURNAL_CONSUMER_LEASE_MS: u64 = 300_000;
pub const DEFAULT_USAGE_JOURNAL_DELETE_BAD_FILES: bool = false;
pub const DEFAULT_USAGE_QUERY_BIND_ADDR: &str = "127.0.0.1:19081";
pub const DEFAULT_USAGE_QUERY_BASE_URL: &str = "http://127.0.0.1:19081";
```

Add matching fields to `AdminRuntimeConfig` and optional fields to `UpdateAdminRuntimeConfig`.

- [ ] **Step 2: Add SQLite migration 7**

Create `0007_usage_journal_runtime_settings.sql` with `ALTER TABLE llm_runtime_config ADD COLUMN` statements for all new config fields. Each numeric field must have a non-negative or minimum-one check matching the default.

Update `llm-access-migrations/src/lib.rs` migration count from 6 to 7 and assert the SQL contains `usage_journal_enabled` and `usage_query_base_url`.

- [ ] **Step 3: Extend store conversion**

Update `RuntimeConfigRecord`, `Default`, `decode_runtime_config`, `to_admin_runtime_config`, `apply_admin_runtime_config`, `upsert_runtime_config`, and repository tests so runtime config round-trips the new fields.

- [ ] **Step 4: Add frontend API types**

Add to `frontend/src/api.rs`:

```rust
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
pub struct AdminUsageWorkerProgressView {
    pub state: String,
    pub current_file_path: Option<String>,
    pub current_file_sequence: Option<u64>,
    pub processed_blocks: u64,
    pub total_blocks: u64,
    pub processed_events: u64,
    pub total_events: u64,
    pub processed_compressed_bytes: u64,
    pub total_compressed_bytes: u64,
    pub progress_percent: f64,
    pub import_rate_events_per_second: f64,
    pub heartbeat_age_ms: Option<i64>,
    pub last_successful_file_sequence: Option<u64>,
    pub last_successful_import_at_ms: Option<i64>,
    pub last_error: Option<String>,
    pub last_error_at_ms: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
pub struct AdminUsageJournalStatusView {
    pub journal_enabled: bool,
    pub journal_root: String,
    pub active_file_sequence: Option<u64>,
    pub active_file_bytes: u64,
    pub sealed_file_count: u64,
    pub sealed_bytes: u64,
    pub oldest_sealed_age_ms: Option<i64>,
    pub dropped_files_total: u64,
    pub dropped_unconsumed_files_total: u64,
    pub write_failures_total: u64,
    pub usage_query_base_url: String,
    pub worker: AdminUsageWorkerProgressView,
    pub generated_at: i64,
}
```

Add `fetch_admin_usage_journal_status()`.

- [ ] **Step 5: Verify Task 3**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-migrations sqlite_migrations_are_file_backed_and_versioned --jobs 4
cargo test -p llm-access-store runtime_config_repository_upserts_single_default_record --jobs 4
cargo test -p llm-access-core --jobs 4
```

Expected: config migration and runtime-config tests pass.

- [ ] **Step 6: Commit Task 3**

```bash
git add llm-access-core llm-access-migrations llm-access-store frontend/src/api.rs
git commit -m "feat(llm-access): add usage journal runtime settings"
```

### Task 4: API Producer Journal Sink

**Files:**
- Modify: `llm-access/Cargo.toml`
- Create: `llm-access/src/usage_journal.rs`
- Modify: `llm-access/src/runtime.rs`
- Modify: `llm-access/src/config.rs`
- Modify: `llm-access/src/lib.rs`

- [ ] **Step 1: Add tests for the sink**

Add tests in `llm-access/src/usage_journal.rs`:

```rust
#[tokio::test]
async fn journal_sink_writes_event_without_duckdb() {
    let root = tempfile::tempdir().expect("tempdir");
    let sink = JournalUsageEventSink::open_for_tests(root.path().to_path_buf()).expect("open sink");
    sink.append_usage_event(&test_usage_event("evt-api-journal")).await.expect("append");
    let status = sink.status_snapshot().expect("status");
    assert_eq!(status.active_file_sequence, Some(0));
    assert_eq!(status.write_failures_total, 0);
}

#[tokio::test]
async fn journal_sink_drops_diagnostic_event_on_write_failure() {
    let sink = JournalUsageEventSink::from_writer(FailingJournalWriter);
    sink.append_usage_event(&test_usage_event("evt-drop")).await.expect("diagnostic drop is non-fatal");
    let status = sink.status_snapshot().expect("status");
    assert_eq!(status.write_failures_total, 1);
}
```

- [ ] **Step 2: Implement `JournalUsageEventSink`**

The sink implements `UsageEventSink`. It converts `UsageEvent` into journal records and writes batches through `llm_usage_journal::JournalWriter`.

Write failures increment counters and return `Ok(())` after SQLite rollups are persisted. This preserves the diagnostic-only contract.

- [ ] **Step 3: Rewire `UsageAccounting`**

In `llm-access/src/runtime.rs`, stop passing `duckdb_usage` as the analytics sink for the API process. Pass the journal sink instead:

```rust
let (usage_accounting, usage_event_flusher) =
    UsageAccounting::new(repository.clone(), journal_usage.clone(), runtime_config.clone());
```

Keep `usage_analytics_store` in the API process as either `EmptyUsageAnalyticsStore` or a proxy-backed store after Task 6.

- [ ] **Step 4: Add config parsing**

Extend `StorageConfig` with `usage_journal_dir`. Default to `state_root.join("usage-journal")`. Add a `--usage-journal-dir /var/lib/staticflow/llm-access/usage-journal` CLI argument for deployments that keep hot journal files outside the JuiceFS state root.

- [ ] **Step 5: Verify Task 4**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access journal_sink --jobs 4
cargo test -p llm-access usage_accounting --jobs 4
```

Expected: journal sink tests pass and existing usage accounting tests still pass.

- [ ] **Step 6: Commit Task 4**

```bash
git add llm-access/Cargo.toml llm-access/src/usage_journal.rs llm-access/src/runtime.rs llm-access/src/config.rs llm-access/src/lib.rs
git commit -m "feat(llm-access): write usage events to journal"
```

### Task 5: Usage Worker Consumer And Progress State

**Files:**
- Create: `llm-access/src/usage_worker.rs`
- Create: `llm-access/src/bin/llm-access-usage-worker.rs`
- Modify: `llm-access/src/config.rs`
- Modify: `llm-access/src/lib.rs`
- Modify: `llm-access-store/src/duckdb.rs`
- Modify: `llm-usage-journal/src/state.rs`
- Modify: `llm-usage-journal/src/status.rs`

- [ ] **Step 1: Add progress and idempotency tests**

Add tests:

```rust
#[tokio::test]
async fn worker_imports_sealed_journal_and_deletes_file() {
    let fixture = UsageWorkerFixture::new();
    fixture.write_sealed_event("evt-worker-1");
    fixture.run_one_import().await.expect("import");
    assert!(!fixture.sealed_path(0).exists());
    assert!(fixture.duckdb_event_exists("evt-worker-1"));
}

#[tokio::test]
async fn worker_progress_updates_after_each_committed_block() {
    let fixture = UsageWorkerFixture::new();
    fixture.write_sealed_events_in_two_blocks(["evt-progress-1", "evt-progress-2"]);
    fixture.run_until_first_block_commit().await.expect("first block");
    let progress = fixture.progress_snapshot();
    assert_eq!(progress.state, "importing");
    assert_eq!(progress.processed_blocks, 1);
    assert_eq!(progress.total_blocks, 2);
    assert!(progress.progress_percent > 0.0);
}

#[tokio::test]
async fn worker_retry_does_not_duplicate_event_id() {
    let fixture = UsageWorkerFixture::new();
    fixture.write_sealed_event("evt-idempotent");
    fixture.simulate_commit_before_delete().await.expect("commit");
    fixture.run_one_import().await.expect("retry");
    assert_eq!(fixture.count_duckdb_event("evt-idempotent"), 1);
}
```

- [ ] **Step 2: Implement consumer state database**

In `llm-usage-journal/src/state.rs`, create `consumer-state.sqlite3` with:

```sql
CREATE TABLE IF NOT EXISTS consumed_files (
    file_sequence INTEGER PRIMARY KEY,
    file_digest TEXT NOT NULL,
    event_count INTEGER NOT NULL,
    imported_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS worker_progress (
    id TEXT PRIMARY KEY CHECK (id = 'current'),
    state TEXT NOT NULL,
    current_file_path TEXT,
    current_file_sequence INTEGER,
    processed_blocks INTEGER NOT NULL,
    total_blocks INTEGER NOT NULL,
    processed_events INTEGER NOT NULL,
    total_events INTEGER NOT NULL,
    processed_compressed_bytes INTEGER NOT NULL,
    total_compressed_bytes INTEGER NOT NULL,
    heartbeat_at_ms INTEGER,
    last_error TEXT,
    last_error_at_ms INTEGER,
    updated_at_ms INTEGER NOT NULL
);
```

- [ ] **Step 3: Implement worker import loop**

The worker claims the oldest sealed file, validates CRC/footer, imports block batches into `DuckDbUsageRepository::open_tiered_with_connection_config`, updates progress after each block commit, records consumed file metadata, and deletes the claimed file after commit.

For idempotency, add a DuckDB repository helper that filters out event ids already present in the active or archived tiered store before inserting a batch.

- [ ] **Step 4: Verify Task 5**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access worker_imports_sealed_journal_and_deletes_file --jobs 4
cargo test -p llm-access worker_progress_updates_after_each_committed_block --jobs 4
cargo test -p llm-access worker_retry_does_not_duplicate_event_id --jobs 4
```

Expected: worker import, progress, and idempotency tests pass.

- [ ] **Step 5: Commit Task 5**

```bash
git add llm-access/src/usage_worker.rs llm-access/src/bin/llm-access-usage-worker.rs llm-access/src/config.rs llm-access/src/lib.rs llm-access-store/src/duckdb.rs llm-usage-journal/src/state.rs llm-usage-journal/src/status.rs
git commit -m "feat(llm-access): consume usage journals in worker"
```

### Task 6: Usage Query Service And API Compatibility Proxy

**Files:**
- Create: `llm-access/src/usage_query.rs`
- Modify: `llm-access/src/usage_worker.rs`
- Modify: `llm-access/src/admin.rs`
- Modify: `llm-access/src/lib.rs`
- Modify: `llm-access/src/runtime.rs`

- [ ] **Step 1: Add route compatibility tests**

Add router tests:

```rust
#[tokio::test]
async fn usage_worker_serves_legacy_llm_usage_paths() {
    let app = usage_worker_test_router_with_event("evt-query-1");
    let response = get_json(&app, "/admin/llm-gateway/usage?limit=1").await;
    assert_eq!(response["events"][0]["id"], "evt-query-1");
}

#[tokio::test]
async fn api_proxy_returns_503_when_usage_worker_is_unreachable() {
    let app = api_router_with_usage_query_base("http://127.0.0.1:9");
    let response = request(&app, "/admin/llm-gateway/usage").await;
    assert_eq!(response.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn combined_journal_status_returns_producer_state_when_worker_is_unreachable() {
    let app = api_router_with_usage_query_base("http://127.0.0.1:9");
    let value = get_json(&app, "/admin/llm-access/usage-journal/status").await;
    assert_eq!(value["worker"]["state"], "unreachable");
    assert!(value["journal_root"].as_str().is_some());
}
```

- [ ] **Step 2: Move admin usage response types into a reusable module**

Make the usage list/detail view structs serializable and deserializable in a shared module usable by both API service and worker. Keep the JSON field names identical to the existing frontend contract.

- [ ] **Step 3: Implement worker query routes**

The worker exposes:

```text
GET /admin/llm-gateway/usage
GET /admin/llm-gateway/usage/:event_id
GET /admin/kiro-gateway/usage
GET /admin/kiro-gateway/usage/:event_id
GET /admin/llm-access/usage-worker/status
```

The list/detail handlers call the tiered `UsageAnalyticsStore` directly inside the worker process.

- [ ] **Step 4: Implement API proxy routes**

The API service keeps the same route paths but proxies usage list/detail to `usage_query_base_url`. It preserves query strings, path escaping, response body, and response status. The combined journal status endpoint merges local producer status with proxied worker status.

- [ ] **Step 5: Verify Task 6**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access usage_worker_serves_legacy_llm_usage_paths --jobs 4
cargo test -p llm-access api_proxy_returns_503_when_usage_worker_is_unreachable --jobs 4
cargo test -p llm-access combined_journal_status_returns_producer_state_when_worker_is_unreachable --jobs 4
```

Expected: worker routes and API proxy compatibility tests pass.

- [ ] **Step 6: Commit Task 6**

```bash
git add llm-access/src/usage_query.rs llm-access/src/usage_worker.rs llm-access/src/admin.rs llm-access/src/lib.rs llm-access/src/runtime.rs
git commit -m "feat(llm-access): proxy usage queries to worker"
```

### Task 7: Frontend Journal Status Panel

**Files:**
- Modify: `frontend/src/api.rs`
- Modify: `frontend/src/pages/admin_llm_gateway.rs`

- [ ] **Step 1: Add frontend tests for formatting helpers**

Add tests near existing `admin_llm_gateway.rs` tests:

```rust
#[test]
fn usage_worker_progress_label_formats_importing_state() {
    let progress = AdminUsageWorkerProgressView {
        state: "importing".to_string(),
        processed_events: 50,
        total_events: 100,
        progress_percent: 50.0,
        ..AdminUsageWorkerProgressView::default()
    };
    assert_eq!(usage_worker_progress_label(&progress), "importing 50/100 events (50.0%)");
}

#[test]
fn usage_worker_progress_percent_is_clamped_for_display() {
    assert_eq!(clamp_progress_percent(-1.0), 0.0);
    assert_eq!(clamp_progress_percent(51.2), 51.2);
    assert_eq!(clamp_progress_percent(120.0), 100.0);
}
```

- [ ] **Step 2: Fetch journal status**

Add `usage_journal_status`, `usage_journal_status_loading`, and `usage_journal_status_error` states to `AdminLlmGatewayPage`. Fetch `fetch_admin_usage_journal_status()` during initial load and on usage reload.

- [ ] **Step 3: Render the status panel**

Add a panel above the usage table with:

- backlog cards: sealed files, sealed bytes, oldest sealed age, dropped unconsumed files
- worker card: state, current file sequence, progress bar, processed/total events, processed/total bytes, import rate, heartbeat age
- query compatibility card: usage query base URL and proxy status

Use existing `classes!` style and helper formatting functions. Do not display full journal payloads.

- [ ] **Step 4: Verify Task 7**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p frontend usage_worker_progress --target wasm32-unknown-unknown --jobs 4
```

Expected: frontend helper tests pass.

- [ ] **Step 5: Commit Task 7**

```bash
git add frontend/src/api.rs frontend/src/pages/admin_llm_gateway.rs
git commit -m "feat(frontend): show usage worker progress"
```

### Task 8: Deployment Artifacts And End-To-End Verification

**Files:**
- Modify: `conf/llm-access-cloud-release.env.example`
- Modify: `docs/ops-runbook.md`
- Modify: `scripts/activate_llm_access_cloud_release.sh`

- [ ] **Step 1: Document service split**

Update ops docs with:

```text
llm-access.service:
  bind: 127.0.0.1:19080
  owns: SQLite rollups, provider traffic, journal producer

llm-access-usage-worker.service:
  bind: 127.0.0.1:19081
  owns: journal consumption, tiered DuckDB writes, usage query API
```

- [ ] **Step 2: Add release env examples**

Add env names for:

```text
LLM_ACCESS_USAGE_JOURNAL_DIR=/var/lib/staticflow/llm-access/usage-journal
LLM_ACCESS_USAGE_QUERY_BIND_ADDR=127.0.0.1:19081
LLM_ACCESS_USAGE_QUERY_BASE_URL=http://127.0.0.1:19081
LLM_ACCESS_DUCKDB_ACTIVE_DIR=/var/lib/staticflow/llm-access/analytics-active
LLM_ACCESS_DUCKDB_ARCHIVE_DIR=/mnt/llm-access/analytics/segments
LLM_ACCESS_DUCKDB_CATALOG_DIR=/mnt/llm-access/analytics/catalog
```

- [ ] **Step 3: Run crate checks**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-usage-journal --jobs 4
cargo test -p llm-access-core -p llm-access-store -p llm-access --jobs 4
cargo clippy -p llm-usage-journal -p llm-access-core -p llm-access-store -p llm-access --jobs 4 -- -D warnings
```

Expected: tests pass and clippy emits zero warnings.

- [ ] **Step 4: Format only changed Rust files**

Run `rustfmt` on changed Rust files only. Do not run workspace-root `cargo fmt`.

- [ ] **Step 5: Commit Task 8**

```bash
git add conf/llm-access-cloud-release.env.example docs/ops-runbook.md scripts/activate_llm_access_cloud_release.sh
git commit -m "docs(llm-access): document usage worker deployment"
```

### Task 9: Final Cutover Check

**Files:**
- Inspect only unless verification reveals a defect.

- [ ] **Step 1: Prove API works without worker**

Start API with worker stopped and verify `/healthz`, `/v1/models`, and one provider request path still respond. Verify `/admin/llm-gateway/usage` returns `503`.

- [ ] **Step 2: Prove worker catches up**

Start worker and verify:

```text
/admin/llm-access/usage-journal/status
```

reports sealed backlog decreasing and worker progress advancing.

- [ ] **Step 3: Prove local mirror compatibility**

Verify both forms work:

```text
http://127.0.0.1:19182/admin/kiro-gateway/usage/evt-smoke-from-worker
http://127.0.0.1:19183/admin/kiro-gateway/usage/evt-smoke-from-worker
```

The response bodies must contain the same `id`, `client_request_body_json`, `upstream_request_body_json`, and `full_request_json` values.

- [ ] **Step 4: Final commit if verification required fixes**

If fixes were needed:

```bash
git add llm-access llm-access-core llm-access-store llm-usage-journal frontend/src/api.rs frontend/src/pages/admin_llm_gateway.rs conf/llm-access-cloud-release.env.example docs/ops-runbook.md scripts/activate_llm_access_cloud_release.sh
git commit -m "fix(llm-access): finalize usage journal cutover"
```

If no fixes were needed, do not create an empty commit.
