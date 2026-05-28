# LLM Access Monitoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a recent-window llm-access monitoring page backed by worker-time proxy attribution and a dedicated metrics API without changing existing usage-event JSON.

**Architecture:** Persist proxy attribution into DuckDB rows during worker consumption, cache account-to-proxy resolution in Valkey, aggregate monitoring metrics from hot+tiered DuckDB with catalog-based segment pruning, and expose the result through a new admin route consumed by a dedicated frontend page.

**Tech Stack:** Rust, Axum, DuckDB, Postgres, Valkey/Redis, Yew/WASM

---

### Task 1: Internal proxy-attribution plumbing

**Files:**
- Modify: `llm-access-store/src/request_cache.rs`
- Modify: `llm-access-store/src/postgres.rs`
- Modify: `llm-access/src/bin/llm-access-usage-worker.rs`
- Modify: `llm-access/src/usage_worker.rs`

- [ ] Add cached usage-attribution key/types and generation-aware read/write helpers.
- [ ] Add Postgres account-to-proxy attribution resolution for Codex and Kiro.
- [ ] Thread the concrete Postgres repository into the usage worker for consume-time enrichment.

### Task 2: DuckDB schema and metrics query engine

**Files:**
- Modify: `llm-access-store/src/duckdb.rs`
- Modify: `llm-access-core/src/store.rs`

- [ ] Add persisted proxy-attribution columns to `usage_events` and compaction/legacy-copy SQL.
- [ ] Add a direct `append_usage_event_rows_owned` path so worker-side enrichment does not mutate the public `UsageEvent`.
- [ ] Add recent-window metrics query types and DuckDB aggregation with one scan per opened segment.
- [ ] Cover hot and tiered/archive metrics behavior with tests.

### Task 3: Worker/API HTTP surface

**Files:**
- Create: `llm-access/src/usage_metrics.rs`
- Modify: `llm-access/src/usage_worker.rs`
- Modify: `llm-access/src/admin.rs`
- Modify: `llm-access/src/lib.rs`

- [ ] Add metrics request/response normalization and the worker endpoint.
- [ ] Proxy the new endpoint through llm-access API without touching existing usage serialization.
- [ ] Add route-level tests.

### Task 4: Frontend monitoring page

**Files:**
- Create: `frontend/src/pages/admin_llm_gateway_monitor.rs`
- Modify: `frontend/src/pages/mod.rs`
- Modify: `frontend/src/router.rs`
- Modify: `frontend/src/api.rs`
- Modify: `frontend/src/pages/admin.rs`
- Modify: `frontend/src/pages/admin_llm_gateway.rs`

- [ ] Add frontend API types/fetcher for the metrics endpoint.
- [ ] Add a dedicated admin monitoring route/page with window/provider controls and metric tables.
- [ ] Link the page from existing admin entry points without bloating the current monolithic llm-gateway page further.

### Task 5: Verification and release

**Files:**
- Modify as needed from tasks above only.

- [ ] Run focused Rust tests, clippy, and exact-file rustfmt.
- [ ] Build self-hosted frontend output.
- [ ] Commit and push the code changes.
- [ ] Release updated llm-access API and usage worker to AWS.
- [ ] Verify metrics endpoint, worker health, API proxying, and page-level hotspot timings.
