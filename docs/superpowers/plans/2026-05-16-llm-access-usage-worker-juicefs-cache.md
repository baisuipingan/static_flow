# llm-access Usage Worker JuiceFS Cache Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move worker-owned usage details and archived analytics off direct R2 uploads and onto a dedicated JuiceFS usage mount with read cache + writeback, while keeping API quota/accounting unchanged and making cleanup metadata-only.

**Architecture:** Keep SQLite rollups and hot usage-journal production unchanged in the API path. The worker continues to own usage queries and tiered DuckDB ingestion, but its historical storage moves to a dedicated usage JuiceFS mount and a local-only packed-detail store. Retention becomes day-bucket driven: catalog metadata decides archived segment deletion, bucket directories decide detail deletion, and cleanup never opens JuiceFS `.duckdb` contents.

**Tech Stack:** Rust (`llm-access`, `llm-access-store`, `llm-access-core`), DuckDB, SQLite, JuiceFS/systemd templates, shell release scripts, Axum worker HTTP surface.

---

### Task 1: Split worker storage config from API control root

**Files:**
- Modify: `llm-access/src/config.rs`
- Modify: `llm-access/src/bin/llm-access-usage-worker.rs`
- Modify: `llm-access/src/lib.rs`
- Test: `llm-access/src/config.rs`

- [ ] **Step 1: Write the failing config test for worker state root plus external SQLite control**

```rust
#[test]
fn parses_tiered_worker_config_with_external_sqlite_control() {
    let command = super::CliCommand::parse([
        "llm-access",
        "serve",
        "--state-root",
        "/mnt/llm-access-usage",
        "--sqlite-control",
        "/mnt/llm-access/control/llm-access.sqlite3",
        "--duckdb-active-dir",
        "/var/lib/staticflow/llm-access/analytics-active",
        "--duckdb-archive-dir",
        "/mnt/llm-access-usage/analytics/segments",
        "--duckdb-catalog-dir",
        "/mnt/llm-access-usage/analytics/catalog",
        "--usage-details-dir",
        "/mnt/llm-access-usage/details",
    ])
    .expect("parse worker serve command");

    let super::CliCommand::Serve(config) = command else {
        panic!("expected serve command");
    };
    assert_eq!(config.storage.state_root, PathBuf::from("/mnt/llm-access-usage"));
    assert_eq!(
        config.storage.sqlite_control,
        PathBuf::from("/mnt/llm-access/control/llm-access.sqlite3")
    );
    let tiered = config.storage.duckdb_tiered.expect("tiered config");
    assert_eq!(tiered.archive_dir, PathBuf::from("/mnt/llm-access-usage/analytics/segments"));
    assert_eq!(tiered.catalog_dir, PathBuf::from("/mnt/llm-access-usage/analytics/catalog"));
    assert_eq!(tiered.details_dir, Some(PathBuf::from("/mnt/llm-access-usage/details")));
}
```

- [ ] **Step 2: Run the config test and verify it fails on the old parser**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access parses_tiered_worker_config_with_external_sqlite_control --jobs 4
```

Expected: FAIL because `--usage-details-dir` is unknown and/or `sqlite_control` is rejected for living outside `--state-root`.

- [ ] **Step 3: Replace the tiered details URL with a path field and relax SQLite-root validation for the worker shape**

```rust
pub struct TieredDuckDbStorageConfig {
    pub active_dir: PathBuf,
    pub archive_dir: PathBuf,
    pub catalog_dir: PathBuf,
    pub rollover_bytes: u64,
    pub details_dir: Option<PathBuf>,
}

fn parse_tiered_duckdb_config(
    active_dir: Option<PathBuf>,
    archive_dir: Option<PathBuf>,
    catalog_dir: Option<PathBuf>,
    rollover_bytes: Option<u64>,
    details_dir: Option<PathBuf>,
) -> anyhow::Result<Option<TieredDuckDbStorageConfig>> {
    let any = active_dir.is_some()
        || archive_dir.is_some()
        || catalog_dir.is_some()
        || rollover_bytes.is_some()
        || details_dir.is_some();
    if !any {
        return Ok(None);
    }
    Ok(Some(TieredDuckDbStorageConfig {
        active_dir: active_dir.ok_or_else(|| anyhow!("--duckdb-active-dir is required"))?,
        archive_dir: archive_dir.ok_or_else(|| anyhow!("--duckdb-archive-dir is required"))?,
        catalog_dir: catalog_dir.ok_or_else(|| anyhow!("--duckdb-catalog-dir is required"))?,
        rollover_bytes: rollover_bytes
            .unwrap_or(DEFAULT_TIERED_DUCKDB_ROLLOVER_BYTES)
            .max(1),
        details_dir,
    }))
}
```

Also update the parser arm:

```rust
"--usage-details-dir" => {
    usage_details_dir = Some(PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow!("--usage-details-dir requires a path"))?,
    ));
}
```

And change root validation from “SQLite must live under state root” to:

```rust
if duckdb_tiered.is_none() {
    ensure_under_root(&state_root, &sqlite_control)?;
}
```

- [ ] **Step 4: Thread the renamed `details_dir` field into worker bootstrap**

```rust
let duckdb = if let Some(tiered) = storage.duckdb_tiered {
    DuckDbUsageRepository::open_tiered_with_connection_config(
        TieredDuckDbUsageConfig {
            active_dir: tiered.active_dir,
            archive_dir: tiered.archive_dir,
            catalog_dir: tiered.catalog_dir,
            rollover_bytes: tiered.rollover_bytes,
            details_dir: tiered.details_dir,
        },
        Arc::clone(&connection_config),
    )?
} else {
    DuckDbUsageRepository::open_path_with_connection_config(
        storage.duckdb,
        Arc::clone(&connection_config),
    )?
};
```

Mirror the same rename in `llm-access/src/lib.rs` for `bootstrap_storage()`.

- [ ] **Step 5: Re-run the config test and the existing parser tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access config --jobs 4
```

Expected: PASS, including the new worker-specific tiered config test.

- [ ] **Step 6: Commit the config split**

```bash
git add llm-access/src/config.rs llm-access/src/bin/llm-access-usage-worker.rs llm-access/src/lib.rs
git commit -m "refactor(llm-access): split worker usage storage paths"
```

### Task 2: Make usage detail persistence local-file-only and keep packed detail format

**Files:**
- Modify: `llm-access-store/src/duckdb.rs`
- Modify: `llm-access-store/Cargo.toml`
- Test: `llm-access-store/src/duckdb.rs`

- [ ] **Step 1: Add a failing test that rejects remote detail URLs**

```rust
#[cfg(feature = "duckdb-runtime")]
#[test]
fn tiered_usage_detail_store_rejects_non_file_backends() {
    let err = super::DuckDbUsageRepository::open_tiered(super::TieredDuckDbUsageConfig {
        active_dir: std::env::temp_dir().join("llm-access-active-reject-remote"),
        archive_dir: std::env::temp_dir().join("llm-access-archive-reject-remote"),
        catalog_dir: std::env::temp_dir().join("llm-access-catalog-reject-remote"),
        rollover_bytes: u64::MAX,
        details_dir: Some(std::path::PathBuf::from("s3://should-not-work")),
    })
    .expect_err("non-local details dir must fail");

    assert!(err.to_string().contains("local filesystem path"));
}
```

If you keep `details_dir: PathBuf`, make the invalid case `PathBuf::from("s3://should-not-work")` and reject it inside `from_dir()` by requiring `path.is_absolute()`.

- [ ] **Step 2: Run the failing detail-store test**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store tiered_usage_detail_store_rejects_non_file_backends --jobs 4
```

Expected: FAIL because the repository still expects `details_object_store_url`.

- [ ] **Step 3: Replace URL parsing with a local directory-backed detail store**

```rust
#[derive(Debug, Clone)]
struct UsageEventDetailStore {
    store: Arc<dyn ObjectStore>,
    root_dir: PathBuf,
}

impl UsageEventDetailStore {
    fn from_dir(path: &Path) -> anyhow::Result<Option<Self>> {
        if path.as_os_str().is_empty() {
            return Ok(None);
        }
        if !path.is_absolute() {
            return Err(anyhow!("usage details dir must be an absolute local filesystem path"));
        }
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create usage details dir `{}`", path.display()))?;
        let store = LocalFileSystem::new_with_prefix(path)
            .with_context(|| format!("failed to open usage details dir `{}`", path.display()))?;
        Ok(Some(Self {
            store: Arc::new(store),
            root_dir: path.to_path_buf(),
        }))
    }
}
```

Then rename all `detail_object_store` usages to `detail_store`, and change:

```rust
let detail_store = config
    .details_dir
    .as_deref()
    .map(UsageEventDetailStore::from_dir)
    .transpose()?
    .flatten()
    .map(Arc::new);
```

- [ ] **Step 4: Preserve the current packed-detail relative path format and update tests to use a directory helper**

```rust
#[cfg(feature = "duckdb-runtime")]
fn details_store_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join("usage-details")
}

#[cfg(feature = "duckdb-runtime")]
fn details_store_pack_path(root: &std::path::Path, relative: &str) -> std::path::PathBuf {
    details_store_dir(root).join(relative)
}
```

Update every tiered test config from:

```rust
details_object_store_url: Some(details_store_url(&root)),
```

to:

```rust
details_dir: Some(details_store_dir(&root)),
```

- [ ] **Step 5: Drop the AWS-only object-store feature from the crate manifest**

```toml
[dependencies]
object_store = { version = "0.12.4", features = ["fs"] }
```

If `url = "2.5"` becomes unused after the rename, remove it in the same commit.

- [ ] **Step 6: Re-run the detail-pack tests that protect current behavior**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store \
  duckdb_repository_separates_detail_payloads_from_usage_fact_rows \
  duckdb_repository_writes_heavy_detail_payloads_into_shared_pack \
  duckdb_repository_returns_empty_payloads_when_external_detail_pack_is_missing \
  --jobs 4
```

Expected: PASS with local day-bucketed detail packs and no R2/S3 dependency.

- [ ] **Step 7: Commit the local-only detail backend**

```bash
git add llm-access-store/src/duckdb.rs llm-access-store/Cargo.toml
git commit -m "refactor(llm-access-store): keep usage detail packs local-only"
```

### Task 3: Bucket archived segments by day and prune only from metadata

**Files:**
- Modify: `llm-access-store/src/duckdb.rs`
- Test: `llm-access-store/src/duckdb.rs`

- [ ] **Step 1: Write failing tests for nested archive buckets and expired detail-bucket deletion**

```rust
#[cfg(feature = "duckdb-runtime")]
#[tokio::test]
async fn duckdb_tiered_retention_prunes_nested_archive_day_bucket() {
    let root = std::env::temp_dir().join(format!(
        "llm-access-duckdb-test-{}-nested-retention",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create root");

    let repo = super::DuckDbUsageRepository::open_tiered(super::TieredDuckDbUsageConfig {
        active_dir: root.join("active"),
        archive_dir: root.join("archive"),
        catalog_dir: root.join("catalog"),
        rollover_bytes: 1,
        details_dir: Some(details_store_dir(&root)),
    })
    .expect("open tiered repo");

    let now_ms = 1_700_864_000_000;
    let day_ms = 86_400_000;
    let mut expired = test_usage_event();
    expired.event_id = "expired-nested-segment".to_string();
    expired.created_at_ms = now_ms - 8 * day_ms;
    repo.append_usage_event(&expired).await.expect("append expired");
    wait_for_archived_duckdb_file_count(&root.join("archive"), 1).await;

    let report = repo.prune_usage_analytics(now_ms, 7).await.expect("prune");
    assert_eq!(report.deleted_segments, 1);
    assert_eq!(duckdb_file_count(&root.join("archive")), 0);
}
```

```rust
#[cfg(feature = "duckdb-runtime")]
#[tokio::test]
async fn duckdb_tiered_retention_prunes_expired_detail_day_buckets() {
    let root = std::env::temp_dir().join(format!(
        "llm-access-duckdb-test-{}-detail-retention",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create root");

    let repo = super::DuckDbUsageRepository::open_tiered(super::TieredDuckDbUsageConfig {
        active_dir: root.join("active"),
        archive_dir: root.join("archive"),
        catalog_dir: root.join("catalog"),
        rollover_bytes: u64::MAX,
        details_dir: Some(details_store_dir(&root)),
    })
    .expect("open tiered repo");

    let now_ms = 1_700_864_000_000;
    let day_ms = 86_400_000;
    let mut expired = test_usage_event();
    expired.event_id = "expired-detail-pack".to_string();
    expired.created_at_ms = now_ms - 8 * day_ms;
    expired.client_request_body_json = Some(r#"{"expired":true}"#.to_string());
    expired.upstream_request_body_json = Some(r#"{"expired":true}"#.to_string());
    expired.full_request_json = Some(r#"{"expired":true}"#.to_string());
    repo.append_usage_event(&expired).await.expect("append expired detail");

    let report = repo.prune_usage_analytics(now_ms, 7).await.expect("prune");
    assert!(report.deleted_files >= 1);
    assert!(!details_store_dir(&root).exists() || recursive_file_count(&details_store_dir(&root)) == 0);
}
```

- [ ] **Step 2: Run the new retention tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store \
  duckdb_tiered_retention_prunes_nested_archive_day_bucket \
  duckdb_tiered_retention_prunes_expired_detail_day_buckets \
  --jobs 4
```

Expected: FAIL because archive paths are still flat and prune does not touch details.

- [ ] **Step 3: Bucket archived segments by day using segment stats, not file inspection**

```rust
fn archive_segment_day_dir(config: &TieredDuckDbUsageConfig, stats: &SegmentStats) -> PathBuf {
    let bucket_ts = stats.end_ms.or(stats.start_ms).unwrap_or_else(now_ms);
    let (year, month, day) = utc_date_parts(bucket_ts);
    config
        .archive_dir
        .join(format!("{year:04}-{month:02}-{day:02}"))
}

fn archive_segment_path(
    config: &TieredDuckDbUsageConfig,
    segment_id: &str,
    stats: &SegmentStats,
) -> PathBuf {
    archive_segment_day_dir(config, stats).join(format!("{segment_id}.duckdb"))
}

fn uploading_archive_segment_path(
    config: &TieredDuckDbUsageConfig,
    segment_id: &str,
    stats: &SegmentStats,
) -> PathBuf {
    archive_segment_day_dir(config, stats).join(format!("{segment_id}.uploading.duckdb"))
}
```

Create the bucket dir before renaming the compacted/uploading file:

```rust
fs::create_dir_all(uploading_path.parent().expect("archive bucket dir"))?;
```

- [ ] **Step 4: Replace flat archive scanning with recursive filename-only scanning and add detail-bucket pruning**

```rust
fn walk_duckdb_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut out = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) == Some("duckdb") {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn prune_orphan_archived_duckdb_files(config: &TieredDuckDbUsageConfig) -> anyhow::Result<usize> {
    let referenced = catalog_archived_duckdb_paths(config)?;
    let mut deleted = 0usize;
    for path in walk_duckdb_files(&config.archive_dir)? {
        let name = path.file_name().and_then(|v| v.to_str()).unwrap_or_default();
        if name.ends_with(".uploading.duckdb") || referenced.contains(&path) {
            continue;
        }
        deleted = deleted.saturating_add(remove_duckdb_segment_files(&path)?);
    }
    Ok(deleted)
}
```

For detail buckets, add a metadata-only day prune rooted at `details/packs/`:

```rust
fn prune_expired_detail_buckets(
    detail_root: &Path,
    cutoff_ms: i64,
) -> anyhow::Result<usize> {
    let mut deleted = 0usize;
    let packs_root = detail_root.join("packs");
    for provider_dir in fs::read_dir(&packs_root).ok().into_iter().flat_map(|it| it.filter_map(Result::ok)) {
        let provider_path = provider_dir.path();
        deleted = deleted.saturating_add(prune_provider_detail_buckets(&provider_path, cutoff_ms)?);
    }
    Ok(deleted)
}
```

Make `prune_usage_analytics()` call the new helper after segment pruning.

- [ ] **Step 5: Keep the cleanup contract metadata-only and encode it in comments/tests**

Add a short comment before the new detail prune:

```rust
// Cleanup is metadata-only: we use catalog rows and bucket directory names,
// never opening JuiceFS .duckdb files or detail pack contents to decide deletion.
```

And add one recursive helper that both archive and detail-bucket tests can share:

```rust
fn recursive_file_count(dir: &std::path::Path) -> usize {
    let mut total = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).ok().into_iter().flat_map(|it| it.filter_map(Result::ok)) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                total = total.saturating_add(1);
            }
        }
    }
    total
}

fn duckdb_file_count(dir: &std::path::Path) -> usize {
    walk_duckdb_files(dir).map(|paths| paths.len()).unwrap_or(0)
}
```

- [ ] **Step 6: Re-run the full tiered DuckDB worker-facing test subset**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store \
  duckdb_tiered_retention_prunes_expired_archived_segments \
  duckdb_tiered_retention_discards_expired_active_segment \
  duckdb_tiered_retention_prunes_nested_archive_day_bucket \
  duckdb_tiered_retention_prunes_expired_detail_day_buckets \
  duckdb_tiered_rolls_over_existing_oversized_active_before_append \
  --jobs 4
```

Expected: PASS with recursive archive counting and detail-bucket cleanup.

- [ ] **Step 7: Commit the metadata-only retention change**

```bash
git add llm-access-store/src/duckdb.rs
git commit -m "feat(llm-access-store): bucket and prune usage analytics by metadata"
```

### Task 4: Tighten worker maintenance cadence and keep the rollout worker-only

**Files:**
- Modify: `llm-access/src/bin/llm-access-usage-worker.rs`
- Modify: `llm-access/src/usage_worker.rs`
- Test: `llm-access/src/usage_worker.rs`

- [ ] **Step 1: Add a small unit test that locks the maintenance interval to five minutes or less**

```rust
#[cfg(test)]
mod tests {
    use super::USAGE_ANALYTICS_MAINTENANCE_INTERVAL;

    #[test]
    fn usage_worker_maintenance_interval_is_five_minutes_or_less() {
        assert!(USAGE_ANALYTICS_MAINTENANCE_INTERVAL.as_secs() <= 300);
    }
}
```

- [ ] **Step 2: Shorten the maintenance interval to five minutes and keep the immediate first-pass behavior**

```rust
const RUNTIME_CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const USAGE_ANALYTICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60 * 5);
```

Do **not** add a second background thread. The current loop already performs one immediate maintenance pass because `last_maintenance` starts as `None`.

- [ ] **Step 3: Expand maintenance logging to show detail cleanup too**

```rust
if report.deleted_segments > 0
    || report.deleted_files > 0
    || report.deleted_orphan_files > 0
{
    tracing::info!(
        deleted_segments = report.deleted_segments,
        deleted_files = report.deleted_files,
        deleted_orphan_files = report.deleted_orphan_files,
        retention_days = self.usage_analytics_retention_days(),
        "pruned llm access usage analytics"
    );
}
```

If you add `deleted_detail_files` later, thread it into this log line here instead of inventing a second prune log.

- [ ] **Step 4: Re-run the worker test module**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access usage_worker --jobs 4
```

Expected: PASS with no behavior change to legacy usage HTTP routes.

- [ ] **Step 5: Commit the maintenance cadence change**

```bash
git add llm-access/src/bin/llm-access-usage-worker.rs llm-access/src/usage_worker.rs
git commit -m "feat(llm-access): run usage analytics cleanup on a short cadence"
```

### Task 5: Update worker systemd templates, release scripts, and rollout cleanup steps

**Files:**
- Create: `deployment-examples/systemd/llm-access-usage-juicefs.mount.template`
- Modify: `deployment-examples/systemd/llm-access-usage-worker.service.template`
- Modify: `deployment-examples/systemd/staticflow-wait-llm-access-state`
- Modify: `deployment-examples/systemd/README.md`
- Modify: `conf/llm-access-cloud-release.env.example`
- Modify: `scripts/render_llm_access_cloud_bundle.sh`
- Modify: `scripts/test_llm_access_cloud_bundle.sh`
- Modify: `scripts/release_llm_access_cloud_worker_only.sh`
- Modify: `scripts/activate_llm_access_cloud_release.sh`
- Modify: `scripts/test_llm_access_cloud_release_scripts.sh`
- Modify: `docs/ops-runbook.md`

- [ ] **Step 1: Add the dedicated usage JuiceFS mount template**

Create `deployment-examples/systemd/llm-access-usage-juicefs.mount.template` with:

```ini
[Unit]
Description=JuiceFS mount for llm-access usage analytics
After=network-online.target
Wants=network-online.target

[Mount]
What=${JUICEFS_META_URL}
Where=/mnt/llm-access-usage
Type=juicefs
Options=_netdev,allow_other,cache-dir=/var/cache/juicefs/llm-access-usage,cache-size=40960,writeback,attr-cache=1,entry-cache=1,dir-entry-cache=1

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: Rewire the worker service template to use the usage mount and local detail directory**

Replace the worker-specific env/path section with:

```ini
After=network-online.target mnt-llm\x2daccess.mount mnt-llm\x2daccess\x2dusage.mount
RequiresMountsFor=/mnt/llm-access /mnt/llm-access-usage

Environment=LLM_ACCESS_STATE_ROOT=/mnt/llm-access-usage
Environment=LLM_ACCESS_USAGE_JOURNAL_DIR=/var/lib/staticflow/llm-access/usage-journal
Environment=LLM_ACCESS_DUCKDB_ACTIVE_DIR=/var/lib/staticflow/llm-access/analytics-active
Environment=LLM_ACCESS_DUCKDB_ARCHIVE_DIR=/mnt/llm-access-usage/analytics/segments
Environment=LLM_ACCESS_DUCKDB_CATALOG_DIR=/mnt/llm-access-usage/analytics/catalog
Environment=LLM_ACCESS_USAGE_DETAILS_DIR=/mnt/llm-access-usage/details
WorkingDirectory=/mnt/llm-access-usage
ExecStartPre=/usr/local/bin/staticflow-wait-llm-access-state analytics
ExecStart=/usr/local/bin/llm-access-usage-worker serve --bind ${LLM_ACCESS_USAGE_QUERY_BIND_ADDR} --state-root ${LLM_ACCESS_STATE_ROOT} --sqlite-control ${LLM_ACCESS_SQLITE_CONTROL} --usage-journal-dir ${LLM_ACCESS_USAGE_JOURNAL_DIR} --duckdb-active-dir ${LLM_ACCESS_DUCKDB_ACTIVE_DIR} --duckdb-archive-dir ${LLM_ACCESS_DUCKDB_ARCHIVE_DIR} --duckdb-catalog-dir ${LLM_ACCESS_DUCKDB_CATALOG_DIR} --duckdb-rollover-bytes ${LLM_ACCESS_DUCKDB_ROLLOVER_BYTES} --usage-details-dir ${LLM_ACCESS_USAGE_DETAILS_DIR}
```

- [ ] **Step 3: Teach the wait helper and bundle scripts about the second mount**

Update `staticflow-wait-llm-access-state` so analytics mode can wait on the worker mount by honoring the worker-overridden `LLM_ACCESS_STATE_ROOT`. Keep the API path unchanged.

The key behavior should remain:

```bash
mount_point="${LLM_ACCESS_STATE_ROOT:-${JUICEFS_MOUNT_POINT:-/mnt/llm-access}}"
sqlite_path="${LLM_ACCESS_SQLITE_CONTROL:-${mount_point}/control/llm-access.sqlite3}"
```

Then update `scripts/render_llm_access_cloud_bundle.sh` and `scripts/test_llm_access_cloud_bundle.sh` to copy/assert:

```bash
cp "$ROOT_DIR/deployment-examples/systemd/llm-access-usage-juicefs.mount.template" \
  "$OUT_DIR/mnt-llm\\x2daccess\\x2dusage.mount"
```

And assert:

```bash
grep -F 'RequiresMountsFor=/mnt/llm-access /mnt/llm-access-usage' "$OUT_DIR/llm-access-usage-worker.service"
grep -F 'cache-dir=/var/cache/juicefs/llm-access-usage' "$OUT_DIR/mnt-llm\\x2daccess\\x2dusage.mount"
grep -F -- '--usage-details-dir ${LLM_ACCESS_USAGE_DETAILS_DIR}' "$OUT_DIR/llm-access-usage-worker.service"
```

- [ ] **Step 4: Extend the worker-only release path so it can stage/install the new mount unit**

In `scripts/release_llm_access_cloud_worker_only.sh`, copy the additional rendered mount unit:

```bash
scp "${SSH_OPTS[@]}" \
  "$RENDER_DIR/llm-access-usage-worker.service" \
  "$RENDER_DIR/mnt-llm\\x2daccess\\x2dusage.mount" \
  "$GCP_DEST:$REMOTE_RELEASE_DIR/"
```

In `scripts/activate_llm_access_cloud_release.sh`, add variables for the staged/install mount path:

```bash
USAGE_MOUNT_UNIT_INSTALL_PATH="${LLM_ACCESS_USAGE_MOUNT_UNIT_INSTALL_PATH:-/etc/systemd/system/mnt-llm\\x2daccess\\x2dusage.mount}"
STAGED_USAGE_MOUNT_UNIT="${LLM_ACCESS_STAGED_USAGE_MOUNT_UNIT:-}"
```

Install it when `ACTIVATE_TARGET` includes `worker`, reload systemd, and ensure:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now mnt-llm\\x2daccess\\x2dusage.mount
findmnt -T /mnt/llm-access-usage >/dev/null || fail "/mnt/llm-access-usage is not mounted"
```

- [ ] **Step 5: Rewrite the runbook/config example around worker-local usage storage and explicit cleanup commands**

Update `conf/llm-access-cloud-release.env.example` to keep API values intact and add only the worker-facing path settings that are still shared:

```dotenv
LLM_ACCESS_USAGE_QUERY_BIND_ADDR=127.0.0.1:19081
LLM_ACCESS_DUCKDB_ACTIVE_DIR=/var/lib/staticflow/llm-access/analytics-active
LLM_ACCESS_DUCKDB_ROLLOVER_BYTES=67108864
```

Move the hard-coded usage-mount paths into the worker unit template instead of the shared env example.

In `docs/ops-runbook.md`, add the rollout cleanup block:

```bash
sudo systemctl stop llm-access-usage-worker.service
sudo rm -rf /mnt/llm-access-usage/analytics /mnt/llm-access-usage/details
sudo rm -rf /var/lib/staticflow/llm-access/analytics-active/*
sudo find /var/lib/staticflow/llm-access/usage-journal/sealed -type f -delete
sudo find /var/lib/staticflow/llm-access/usage-journal/consuming -type f -delete
sudo find /var/lib/staticflow/llm-access/usage-journal/bad -type f -delete
sudo systemctl start llm-access-usage-worker.service
```

Do **not** delete `usage-journal/active` while the API service is still producing journal files.

- [ ] **Step 6: Re-run the shell/bundle validation scripts**

Run:

```bash
bash scripts/test_llm_access_cloud_bundle.sh
bash scripts/test_llm_access_cloud_release_scripts.sh
```

Expected: PASS with the second mount unit and worker-only activation wiring.

- [ ] **Step 7: Commit the worker deployment wiring**

```bash
git add \
  deployment-examples/systemd/llm-access-usage-juicefs.mount.template \
  deployment-examples/systemd/llm-access-usage-worker.service.template \
  deployment-examples/systemd/staticflow-wait-llm-access-state \
  deployment-examples/systemd/README.md \
  conf/llm-access-cloud-release.env.example \
  scripts/render_llm_access_cloud_bundle.sh \
  scripts/test_llm_access_cloud_bundle.sh \
  scripts/release_llm_access_cloud_worker_only.sh \
  scripts/activate_llm_access_cloud_release.sh \
  scripts/test_llm_access_cloud_release_scripts.sh \
  docs/ops-runbook.md
git commit -m "feat(llm-access): isolate usage worker storage on cached JuiceFS"
```

### Task 6: Final verification, worker-only release, and GCP data purge

**Files:**
- Modify: none
- Test: runtime verification on the target host

- [ ] **Step 1: Format changed Rust files only**

Run:

```bash
rustfmt llm-access/src/config.rs \
  llm-access/src/bin/llm-access-usage-worker.rs \
  llm-access/src/lib.rs \
  llm-access/src/usage_worker.rs \
  llm-access-store/src/duckdb.rs
```

- [ ] **Step 2: Run the full affected test/clippy suite**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-core -p llm-access-store -p llm-access --jobs 4
cargo clippy -p llm-access-core -p llm-access-store -p llm-access --jobs 4 -- -D warnings
git diff --check
```

Expected: all PASS, zero clippy warnings, clean diff check.

- [ ] **Step 3: Build the release bundle and ship the worker-only rollout**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
./scripts/prepare_llm_access_cloud_release.sh
./scripts/release_llm_access_cloud_worker_only.sh
```

Expected: staged worker binary plus updated worker systemd unit and usage mount unit uploaded to GCP.

- [ ] **Step 4: Purge old worker-owned usage data on GCP before the new worker starts consuming again**

Run on GCP:

```bash
sudo systemctl stop llm-access-usage-worker.service
sudo rm -rf /mnt/llm-access/analytics/segments/*
sudo rm -rf /mnt/llm-access/analytics/catalog/*
sudo rm -rf /mnt/llm-access-usage/*
sudo rm -rf /var/lib/staticflow/llm-access/analytics-active/*
sudo find /var/lib/staticflow/llm-access/usage-journal/sealed -type f -delete
sudo find /var/lib/staticflow/llm-access/usage-journal/consuming -type f -delete
sudo find /var/lib/staticflow/llm-access/usage-journal/bad -type f -delete
```

Expected: old archived DuckDB, old catalog, old usage details, and stale worker backlog are gone; the API remains running.

- [ ] **Step 5: Verify the worker-only rollout and state reset**

Run on GCP:

```bash
findmnt -T /mnt/llm-access >/dev/null
findmnt -T /mnt/llm-access-usage >/dev/null
curl -fsS http://127.0.0.1:19081/admin/llm-access/usage-worker/status
curl -fsS -H 'Host: localhost' http://127.0.0.1:19080/admin/llm-access/usage-journal/status
systemctl show llm-access-usage-worker.service -p ActiveState -p SubState -p ExecStart -p Environment --no-pager
du -sh /mnt/llm-access /mnt/llm-access-usage /var/lib/staticflow/llm-access
```

Expected:

- worker `ActiveState=active`
- `journal_root=/var/lib/staticflow/llm-access/usage-journal`
- archive/catalog/details paths point at `/mnt/llm-access-usage`
- disk usage drops versus the pre-reset state

- [ ] **Step 6: Publish the operator note that this was worker-only**

Use this exact release note sentence in the final handoff:

```text
Only the worker needed updating for this rollout; the API service binary was left unchanged.
```
