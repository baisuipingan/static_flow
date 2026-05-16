# llm-access Single-Core Primary and Edge-Secondary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a single `core` primary plus multi-`edge` secondary `llm-access` topology where only the primary performs refresh and usage ingestion, edge workers proxy usage queries and relay sealed journals, and proxy metadata joins the existing `llma:*` Valkey cache.

**Architecture:** Extend the current Postgres-plus-Valkey runtime with explicit node identity and node class, add a Postgres-backed primary lease plus Valkey-backed cluster read view, split the usage worker into `primary` and `edge-secondary` modes, and keep the API contract stable by preserving local `usage_query_base_url` while making secondaries proxy to the primary worker. Keep JuiceFS required only on `core` nodes and keep request handling decoupled from synchronous primary availability by preserving local journal writes.

**Tech Stack:** Rust (`axum`, `tokio`, `serde`, `reqwest`, `redis`, `tokio-postgres`), existing `llm-access` / `llm-access-store` / `llm-usage-journal` crates, Postgres control plane, Valkey shared cache, existing Yew admin frontend.

---

## File map

### New files expected

- `llm-access-store/src/cluster.rs`
  - Postgres cluster registry and primary lease helper types.
- `llm-access/src/cluster.rs`
  - Runtime node identity, node role state machine, Valkey heartbeat snapshot publishing, and primary discovery helpers.

### Existing files to modify

- `llm-access-core/src/store.rs`
  - Add cluster/node-facing DTOs reused by API, worker, and frontend JSON.
- `llm-access/src/config.rs`
  - Parse node identity / node class / worker mode configuration.
- `llm-access/src/runtime.rs`
  - Wire node identity and role state into runtime construction.
- `llm-access/src/lib.rs`
  - Gate background refresh on primary role instead of static env-only behavior.
- `llm-access/src/usage_worker.rs`
  - Add worker operating mode split: primary ingest vs secondary proxy/relay.
- `llm-access/src/public.rs`
  - Attach usage-source headers or metadata to proxied responses.
- `llm-access/src/admin.rs`
  - Expose cluster-aware worker/journal status and node metadata to admin pages.
- `llm-access/src/bin/llm-access-usage-worker.rs`
  - Start worker in role-aware mode and configure relay/proxy behavior.
- `llm-access-store/src/request_cache.rs`
  - Add `llma:cluster:*` and `llma:proxy:*` cache keys and TTL helpers.
- `llm-access-store/src/postgres.rs`
  - Implement cluster registry / primary lease / proxy metadata cache coverage.
- `llm-usage-journal/src/state.rs`
  - Extend consumed-file identity to include source node and digest.
- `frontend/src/api.rs`
  - Parse node-aware usage metadata headers / response fields.
- `frontend/src/pages/admin_llm_gateway.rs`
  - Show current node role / primary / usage source banner on usage views.
- `docs/ops-runbook.md`
  - Document `core` vs `edge` deployment shape and new env vars.

### Primary test targets

- `cargo test -p llm-access-store --jobs 4`
- `cargo test -p llm-access --jobs 4`
- `cargo test -p llm-usage-journal --jobs 4`
- `cargo test -p static-flow-frontend --jobs 4`
- `cargo clippy -p llm-access-store -p llm-access -p llm-usage-journal -p static-flow-frontend --jobs 4 -- -D warnings`

---

### Task 1: Add node identity and primary-role runtime model

**Files:**
- Create: `llm-access/src/cluster.rs`
- Modify: `llm-access/src/config.rs`
- Modify: `llm-access/src/runtime.rs`
- Modify: `llm-access/src/lib.rs`
- Modify: `llm-access-core/src/store.rs`
- Test: `llm-access/src/lib.rs`

- [ ] **Step 1: Write the failing role-gating tests**

Add tests covering:
- a `core` node with no current primary becomes `primary`;
- an `edge` node never self-promotes;
- refresh loops only start on `primary`.

Example test additions in `llm-access/src/lib.rs`:

```rust
#[test]
fn node_class_parsing_rejects_unknown_values() {
    assert!(parse_node_class("core").is_ok());
    assert!(parse_node_class("edge").is_ok());
    assert!(parse_node_class("weird").is_err());
}

#[test]
fn background_refresh_requires_primary_runtime_role() {
    assert!(background_refresh_should_run(NodeRuntimeRole::Primary, Some("1")));
    assert!(!background_refresh_should_run(NodeRuntimeRole::EdgeSecondary, Some("1")));
    assert!(!background_refresh_should_run(NodeRuntimeRole::Degraded, Some("1")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access background_refresh_requires_primary_runtime_role node_class_parsing_rejects_unknown_values --jobs 4
```

Expected:
- compile failure or missing symbol errors for `NodeRuntimeRole`, `parse_node_class`, or `background_refresh_should_run`.

- [ ] **Step 3: Add the minimal node identity model**

Implement:
- `NodeClass` enum: `Core`, `Edge`
- `NodeRuntimeRole` enum: `Primary`, `EdgeSecondary`, `Degraded`
- `NodeIdentity` struct with `node_id`, `node_class`, display/base URLs
- config parsing for:
  - `LLM_ACCESS_NODE_ID`
  - `LLM_ACCESS_NODE_CLASS`
  - `LLM_ACCESS_NODE_DISPLAY_NAME`
  - `LLM_ACCESS_NODE_REGION`
  - `LLM_ACCESS_API_BASE_URL`
  - `LLM_ACCESS_WORKER_BASE_URL`

Key code shape in `llm-access/src/cluster.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeClass {
    Core,
    Edge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRuntimeRole {
    Primary,
    EdgeSecondary,
    Degraded,
}

pub fn parse_node_class(raw: &str) -> anyhow::Result<NodeClass> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "core" => Ok(NodeClass::Core),
        "edge" => Ok(NodeClass::Edge),
        _ => anyhow::bail!("unsupported llm-access node class"),
    }
}
```

In `llm-access/src/lib.rs`, replace the current env-only gating helper with a role-aware helper:

```rust
fn background_refresh_should_run(role: NodeRuntimeRole, raw: Option<&str>) -> bool {
    if role != NodeRuntimeRole::Primary {
        return false;
    }
    let Some(raw) = raw else {
        return true;
    };
    !matches!(raw.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off")
}
```

- [ ] **Step 4: Re-run targeted tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access background_refresh_requires_primary_runtime_role node_class_parsing_rejects_unknown_values --jobs 4
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add llm-access/src/cluster.rs llm-access/src/config.rs llm-access/src/runtime.rs llm-access/src/lib.rs llm-access-core/src/store.rs
git commit -m "feat: add llm-access node identity model"
```

---

### Task 2: Add Postgres cluster registry and primary lease

**Files:**
- Create: `llm-access-store/src/cluster.rs`
- Modify: `llm-access-store/src/postgres.rs`
- Modify: `llm-access-core/src/store.rs`
- Modify: `llm-access/src/runtime.rs`
- Test: `llm-access-store/src/postgres.rs`

- [ ] **Step 1: Write failing cluster-registry tests**

Add tests that describe:
- runtime self-upsert of a node row;
- `core` node can acquire the primary lease;
- `edge` node is rejected from lease acquisition;
- lease loss makes role recomputation possible.

Example test skeleton:

```rust
#[tokio::test]
async fn postgres_cluster_registry_upserts_node_snapshot() {
    let repo = test_postgres_repo().await;
    repo.upsert_cluster_node(&sample_node("node-a", "core")).await.expect("upsert");
    let nodes = repo.list_cluster_nodes().await.expect("list");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node_id, "node-a");
}

#[tokio::test]
async fn edge_node_cannot_acquire_primary_lease() {
    let repo = test_postgres_repo().await;
    let err = repo.try_acquire_primary_lease("node-edge", NodeClass::Edge).await.unwrap_err();
    assert!(format!("{err:#}").contains("edge node is not primary-eligible"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store postgres_cluster_registry_upserts_node_snapshot edge_node_cannot_acquire_primary_lease --jobs 4
```

Expected:
- missing method / missing type failures around cluster registry and primary lease APIs.

- [ ] **Step 3: Implement the minimal Postgres cluster registry**

Add a focused cluster helper module and repository methods:
- `upsert_cluster_node`
- `list_cluster_nodes`
- `load_primary_snapshot`
- `try_acquire_primary_lease`
- `publish_primary_snapshot`

Prefer a dedicated advisory-lock connection helper in `llm-access-store/src/cluster.rs`:

```rust
pub struct PrimaryLeaseGuard {
    node_id: String,
    _client: tokio_postgres::Client,
}

pub async fn try_acquire_primary_lease(
    pool: &deadpool_postgres::Pool,
    node_id: &str,
    node_class: NodeClass,
) -> anyhow::Result<Option<PrimaryLeaseGuard>> {
    anyhow::ensure!(node_class == NodeClass::Core, "edge node is not primary-eligible");
    // open dedicated connection and pg_try_advisory_lock(...)
}
```

Wire this into runtime startup so role resolution becomes:
- `core + acquired lease => Primary`
- `edge + known primary => EdgeSecondary`
- anything without a usable primary snapshot => `Degraded`

- [ ] **Step 4: Re-run targeted tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store postgres_cluster_registry_upserts_node_snapshot edge_node_cannot_acquire_primary_lease --jobs 4
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add llm-access-store/src/cluster.rs llm-access-store/src/postgres.rs llm-access-core/src/store.rs llm-access/src/runtime.rs
git commit -m "feat: add llm-access cluster registry and primary lease"
```

---

### Task 3: Add Valkey cluster metadata read view and proxy metadata cache

**Files:**
- Modify: `llm-access-store/src/request_cache.rs`
- Modify: `llm-access-store/src/postgres.rs`
- Test: `llm-access-store/src/request_cache.rs`
- Test: `llm-access-store/src/postgres.rs`

- [ ] **Step 1: Write failing cache-key tests**

Add request-cache tests for:
- `llma:cluster:primary`
- `llma:cluster:nodes`
- `llma:cluster:node:<id>`
- `llma:proxy:configs`
- `llma:proxy:binding:codex`
- `llma:proxy:binding:kiro`

Example:

```rust
#[test]
fn cluster_and_proxy_cache_key_namespace_is_stable() {
    let cache = sample_request_cache();
    assert_eq!(cache.cluster_primary_key(), "llma:test:cluster:primary");
    assert_eq!(cache.cluster_node_key("node-a"), "llma:test:cluster:node:node-a");
    assert_eq!(cache.proxy_configs_key(), "llma:test:proxy:configs");
    assert_eq!(cache.proxy_binding_key("codex"), "llma:test:proxy:binding:codex");
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store cluster_and_proxy_cache_key_namespace_is_stable --jobs 4
```

Expected:
- missing key helper methods or missing cache structs.

- [ ] **Step 3: Implement cluster snapshot and proxy metadata cache coverage**

In `llm-access-store/src/request_cache.rs`, add:
- cluster key helpers
- short TTL helpers for cluster snapshots
- cached structs for:
  - primary snapshot
  - node snapshot list
  - proxy config list
  - provider binding snapshot

In `llm-access-store/src/postgres.rs`, change the proxy-resolution context load path:

```rust
async fn load_provider_proxy_resolution_context(
    &self,
    provider_type: &str,
) -> anyhow::Result<ProviderProxyResolutionContext> {
    if let Some(cache) = &self.request_cache {
        if let (Some(configs), Some(binding)) = (
            cache.get_json::<Vec<AdminProxyConfig>>(&cache.proxy_configs_key()).await?,
            cache.get_json::<AdminProxyBinding>(&cache.proxy_binding_key(provider_type)).await?,
        ) {
            return Ok(build_proxy_context_from_cached(configs, binding));
        }
    }
    // existing Postgres fallback + cache repopulation
}
```

Also invalidate the new proxy keys after:
- create / patch / delete proxy config
- update proxy binding
- import legacy kiro proxy configs

- [ ] **Step 4: Re-run targeted tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store cluster_and_proxy_cache_key_namespace_is_stable --jobs 4
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add llm-access-store/src/request_cache.rs llm-access-store/src/postgres.rs
git commit -m "feat: cache llm-access cluster and proxy metadata"
```

---

### Task 4: Split usage worker into primary mode and edge relay/proxy mode

**Files:**
- Modify: `llm-access/src/usage_worker.rs`
- Modify: `llm-access/src/bin/llm-access-usage-worker.rs`
- Modify: `llm-usage-journal/src/state.rs`
- Modify: `llm-access/src/admin.rs`
- Test: `llm-access/src/usage_worker.rs`
- Test: `llm-usage-journal/src/state.rs`

- [ ] **Step 1: Write failing worker-mode tests**

Add tests for:
- secondary worker proxies status/query calls to primary;
- secondary worker relays a sealed journal file and deletes it only after ack;
- consumed-file identity uses `(source_node_id, file_sequence, file_digest)`.

Example test skeleton:

```rust
#[tokio::test]
async fn secondary_worker_relays_sealed_file_to_primary_before_delete() {
    let fixture = SecondaryRelayFixture::new().await;
    fixture.write_sealed_event("evt-relay-1");
    fixture.run_one_import().await.expect("relay import");
    assert!(fixture.primary_received_event("evt-relay-1").await);
    assert!(!fixture.local_sealed_path(0).exists());
}

#[test]
fn consumer_state_distinguishes_same_sequence_from_different_nodes() {
    let state = test_consumer_state();
    state.record_consumed_file("node-a", 7, "digest-a", 1, 1).expect("a");
    state.record_consumed_file("node-b", 7, "digest-b", 1, 1).expect("b");
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access secondary_worker_relays_sealed_file_to_primary_before_delete --jobs 4
cargo test -p llm-usage-journal consumer_state_distinguishes_same_sequence_from_different_nodes --jobs 4
```

Expected:
- missing worker role mode, missing relay path, and missing node-aware consumed-file API.

- [ ] **Step 3: Implement worker role split and idempotent relay ingest**

Add a worker mode enum:

```rust
pub enum UsageWorkerMode {
    Primary,
    EdgeSecondary {
        primary_worker_base_url: String,
        node_id: String,
    },
}
```

Implement in `usage_worker.rs`:
- primary mode keeps existing local ingest behavior;
- secondary mode:
  - query/status handlers proxy to primary;
  - `run_one_import()` relays the sealed file to a new internal ingest path on primary;
  - local file deletion happens only after ack.

Update `llm-usage-journal/src/state.rs` so consumed identity becomes compound:

```rust
pub fn is_consumed(&self, source_node_id: &str, file_sequence: u64, file_digest: &str) -> Result<bool>
pub fn record_consumed_file(
    &self,
    source_node_id: &str,
    file_sequence: u64,
    file_digest: &str,
    event_count: u64,
    imported_at_ms: i64,
) -> Result<()>
```

The primary ingest ledger must reject duplicate append for the same
`(source_node_id, file_sequence, file_digest)`.

- [ ] **Step 4: Re-run targeted tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access secondary_worker_relays_sealed_file_to_primary_before_delete --jobs 4
cargo test -p llm-usage-journal consumer_state_distinguishes_same_sequence_from_different_nodes --jobs 4
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add llm-access/src/usage_worker.rs llm-access/src/bin/llm-access-usage-worker.rs llm-usage-journal/src/state.rs llm-access/src/admin.rs
git commit -m "feat: add llm-access secondary worker relay mode"
```

---

### Task 5: Expose node-aware usage metadata to API and frontend

**Files:**
- Modify: `llm-access/src/public.rs`
- Modify: `llm-access/src/admin.rs`
- Modify: `frontend/src/api.rs`
- Modify: `frontend/src/pages/admin_llm_gateway.rs`
- Test: `llm-access/src/lib.rs`
- Test: `frontend/src/pages/admin_llm_gateway.rs`

- [ ] **Step 1: Write failing response-metadata tests**

Add tests that show:
- usage responses from secondary include node/primary/source metadata headers;
- admin worker status response includes node role and usage source fields.

Example:

```rust
#[tokio::test]
async fn public_usage_proxy_response_sets_cluster_headers() {
    let response = test_usage_proxy_response().await;
    assert_eq!(response.headers()["x-llm-access-usage-source"], "proxied_primary");
    assert_eq!(response.headers()["x-llm-access-worker-role"], "edge-secondary");
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access public_usage_proxy_response_sets_cluster_headers --jobs 4
```

Expected:
- missing headers / missing fields.

- [ ] **Step 3: Implement node-aware response metadata and admin banner data**

In `llm-access/src/public.rs` and `llm-access/src/admin.rs`:
- attach:
  - `x-llm-access-node-id`
  - `x-llm-access-node-class`
  - `x-llm-access-worker-role`
  - `x-llm-access-primary-node-id`
  - `x-llm-access-usage-source`
- extend admin worker/journal status JSON with:
  - current node id
  - node class
  - runtime role
  - primary node id
  - usage query mode
  - local backlog counts

In `frontend/src/api.rs`, parse these headers into a small `UsageRouteMetadata`
view model.

In `frontend/src/pages/admin_llm_gateway.rs`, render a compact status banner
above usage views showing:
- current node
- role
- primary
- usage source
- backlog counts

- [ ] **Step 4: Re-run targeted tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access public_usage_proxy_response_sets_cluster_headers --jobs 4
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add llm-access/src/public.rs llm-access/src/admin.rs frontend/src/api.rs frontend/src/pages/admin_llm_gateway.rs
git commit -m "feat: surface llm-access node-aware usage metadata"
```

---

### Task 6: Update ops docs and run full verification

**Files:**
- Modify: `docs/ops-runbook.md`

- [ ] **Step 1: Document core vs edge deployment**

Add a new runbook section covering:
- required env vars
- which node class mounts JuiceFS
- which service responsibilities belong only to primary
- how to inspect current node role
- what degraded mode means on edge nodes

- [ ] **Step 2: Run focused package tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store -p llm-access -p llm-usage-journal -p static-flow-frontend --jobs 4
```

Expected:
- PASS

- [ ] **Step 3: Run clippy**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo clippy -p llm-access-store -p llm-access -p llm-usage-journal -p static-flow-frontend --jobs 4 -- -D warnings
```

Expected:
- PASS with zero warnings

- [ ] **Step 4: Format changed files**

Run:

```bash
rustfmt llm-access/src/cluster.rs \
  llm-access/src/config.rs \
  llm-access/src/runtime.rs \
  llm-access/src/lib.rs \
  llm-access-store/src/cluster.rs \
  llm-access-store/src/request_cache.rs \
  llm-access-store/src/postgres.rs \
  llm-access/src/usage_worker.rs \
  llm-access/src/bin/llm-access-usage-worker.rs \
  llm-access/src/public.rs \
  llm-access/src/admin.rs \
  llm-access-core/src/store.rs \
  llm-usage-journal/src/state.rs
```

Expected:
- formatting succeeds without touching unrelated workspace crates.

- [ ] **Step 5: Final commit**

```bash
git add docs/ops-runbook.md llm-access llm-access-core llm-access-store llm-usage-journal frontend
git commit -m "feat: add llm-access single-core primary topology"
```

---

## Plan self-review

### Spec coverage

Covered:
- single `core` primary and `edge` secondaries: Tasks 1-2
- automatic role discovery with "first eligible node becomes primary": Task 2
- secondary worker query proxy and journal relay: Task 4
- frontend machine-awareness and usage source display: Task 5
- proxy metadata Valkey cache coverage: Task 3
- JuiceFS only on `core` nodes and ops/runtime docs: Task 6

Not included by design:
- multi-core automatic failover
- federated multi-node usage truth

### Placeholder scan

No `TODO`, `TBD`, or deferred "add tests later" steps remain. Each task names
exact files and commands.

### Type consistency

Shared names are consistent across tasks:
- `NodeClass`
- `NodeRuntimeRole`
- `UsageWorkerMode`
- cluster metadata keys under `llma:cluster:*`
- proxy metadata keys under `llma:proxy:*`

### Inline execution note

The normal superpowers flow prefers a dedicated worktree and may recommend
subagents. This implementation intentionally does neither because the user
explicitly requested:

- no subagent
- no git worktree
- continue directly in the current session

Master-branch execution is acceptable here because the user explicitly asked to
generate the plan and start implementation immediately in the current workspace.
