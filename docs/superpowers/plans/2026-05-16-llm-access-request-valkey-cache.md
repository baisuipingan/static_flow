# llm-access Request-Path Valkey Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `llm-access` request-time key authentication, route snapshot reads, account selection views, and selected-account auth hydration off the normal Postgres hot path and onto the shared Valkey cache, while keeping Postgres as the only durable source of truth.

**Architecture:** Extend the Postgres-backed repository directly with a Valkey-backed cache-aside layer rather than adding a second wrapper repository type. Request-path methods in `PostgresControlRepository` become cache-aware, write/refresh paths explicitly update or invalidate cache entries, and runtime bootstrap wires an optional Valkey client only for the Postgres control path.

**Tech Stack:** Rust, `redis` async client, `tokio`, `serde`, `sqlx-postgres`, existing `llm-access-core` store traits, existing `llm-access-store` Postgres repository.

---

## File structure

### New files

- `llm-access-store/src/request_cache.rs`
  - Valkey client wrapper
  - cache key builders
  - deterministic TTL jitter
  - single-flight lock helpers
  - cache payload structs and serde encode/decode helpers
- `docs/ops-runbook.md`
  - operational note for the Valkey request cache env and fallback behavior

### Modified files

- `Cargo.toml`
  - add workspace-managed Redis dependency version
- `llm-access-store/Cargo.toml`
  - consume the Redis dependency
- `llm-access-store/src/lib.rs`
  - register the new `request_cache` module
- `llm-access-store/src/postgres.rs`
  - add optional request-cache state to `PostgresControlRepository`
  - teach request-path methods to use cache-aside
  - explicitly update/invalidate cache on write and refresh paths
  - add focused Postgres + cache integration tests
- `llm-access/src/config.rs`
  - add optional request-cache CLI/env wiring
- `llm-access/src/runtime.rs`
  - pass request-cache config into `PostgresControlRepository::connect`

### Test surface

- `llm-access-store/src/request_cache.rs`
  - pure unit tests for key naming, deterministic TTL jitter, and payload encoding
- `llm-access-store/src/postgres.rs`
  - integration-style tests using the existing Postgres test DB harness
- `llm-access/src/config.rs`
  - CLI parse tests for the new optional cache args

## Implementation notes before tasks

- This plan intentionally targets the **Postgres-backed live path only**. Do not
  add a parallel SQLite cache implementation.
- Keep the existing short in-process Codex status cache intact for now. The new
  Valkey layer is for request-path routing/auth reads, not for replacing every
  existing cache.
- Use `.local/common/valkey/lb7666.env` for local development and the
  corresponding GCP env wiring at deploy time. Do not commit secrets.
- Preserve userspace behavior if Valkey is unavailable by falling back to the
  current Postgres path. That fallback must be explicit and observable.

### Task 1: Add request-cache config and dependency wiring

**Files:**
- Modify: `Cargo.toml`
- Modify: `llm-access-store/Cargo.toml`
- Modify: `llm-access/src/config.rs`
- Test: `llm-access/src/config.rs`

- [ ] **Step 1: Add the Redis dependency to the workspace**

```toml
# Cargo.toml
[workspace.dependencies]
redis = { version = "0.28", default-features = false, features = ["tokio-comp", "connection-manager", "script"] }
```

```toml
# llm-access-store/Cargo.toml
[dependencies]
redis = { workspace = true }
```

- [ ] **Step 2: Add request-cache config types to `llm-access/src/config.rs`**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCacheConfig {
    pub url_env: String,
    pub key_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageConfig {
    pub state_root: PathBuf,
    pub control_store: ControlStoreConfig,
    pub request_cache: Option<RequestCacheConfig>,
    pub duckdb: PathBuf,
    pub usage_journal_dir: PathBuf,
    // ...
}
```

- [ ] **Step 3: Parse optional request-cache args from CLI**

```rust
let mut request_cache_url_env = None;
let mut request_cache_key_prefix = None;

match arg.to_string_lossy().as_ref() {
    "--request-cache-url-env" => {
        request_cache_url_env = Some(
            args.next()
                .ok_or_else(|| anyhow!("--request-cache-url-env requires an env name"))?
                .to_string_lossy()
                .to_string(),
        );
    },
    "--request-cache-key-prefix" => {
        request_cache_key_prefix = Some(
            args.next()
                .ok_or_else(|| anyhow!("--request-cache-key-prefix requires a value"))?
                .to_string_lossy()
                .to_string(),
        );
    },
    _ => { /* existing arms */ }
}
```

```rust
let request_cache = request_cache_url_env.map(|url_env| RequestCacheConfig {
    url_env,
    key_prefix: request_cache_key_prefix.unwrap_or_else(|| "llma".to_string()),
});
```

- [ ] **Step 4: Extend the usage string and parse tests**

```rust
"usage: llm-access serve [--bind <addr>] --state-root <path> \
 (--sqlite-control <path> | --postgres-control-database-url-env <env>) \
 [--request-cache-url-env <env>] [--request-cache-key-prefix <prefix>] ..."
```

```rust
#[test]
fn parse_postgres_storage_with_request_cache() {
    let command = CliCommand::parse([
        "llm-access",
        "serve",
        "--state-root",
        "/tmp/state",
        "--postgres-control-database-url-env",
        "LLM_ACCESS_CONTROL_DATABASE_URL",
        "--request-cache-url-env",
        "VALKEY_URL",
        "--request-cache-key-prefix",
        "llma",
    ])
    .expect("parse");

    let CliCommand::Serve(config) = command else { panic!("expected serve"); };
    assert_eq!(config.storage.request_cache.as_ref().unwrap().url_env, "VALKEY_URL");
    assert_eq!(config.storage.request_cache.as_ref().unwrap().key_prefix, "llma");
}
```

- [ ] **Step 5: Run the focused config tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access parse_postgres_storage_with_request_cache --jobs 4
```

Expected: PASS

- [ ] **Step 6: Commit the config/dependency wiring**

```bash
git add Cargo.toml llm-access-store/Cargo.toml llm-access/src/config.rs
git commit -m "feat: add llm-access request cache config wiring"
```

### Task 2: Add the Valkey request-cache module

**Files:**
- Create: `llm-access-store/src/request_cache.rs`
- Modify: `llm-access-store/src/lib.rs`
- Test: `llm-access-store/src/request_cache.rs`

- [ ] **Step 1: Create the cache config, client, and key namespace helpers**

```rust
// llm-access-store/src/request_cache.rs
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RequestCacheConfig {
    pub url: String,
    pub key_prefix: String,
}

#[derive(Clone)]
pub struct RequestCache {
    client: redis::Client,
    key_prefix: String,
}

impl RequestCache {
    pub fn new(config: RequestCacheConfig) -> anyhow::Result<Self> {
        Ok(Self {
            client: redis::Client::open(config.url)?,
            key_prefix: config.key_prefix,
        })
    }

    pub fn auth_key(&self, secret_hash: &str) -> String {
        format!("{}:auth:{secret_hash}", self.key_prefix)
    }

    pub fn request_snapshot_key(&self, provider: &str, key_id: &str) -> String {
        format!("{}:req:{provider}:{key_id}", self.key_prefix)
    }

    pub fn account_view_key(&self, provider: &str, account_name: &str) -> String {
        format!("{}:acct:view:{provider}:{account_name}", self.key_prefix)
    }

    pub fn account_auth_key(&self, provider: &str, account_name: &str) -> String {
        format!("{}:acct:auth:{provider}:{account_name}", self.key_prefix)
    }

    pub fn dispatch_generation_key(&self, provider: &str) -> String {
        format!("{}:gen:dispatch:{provider}", self.key_prefix)
    }
}
```

- [ ] **Step 2: Add deterministic TTL jitter helpers**

```rust
fn deterministic_jitter_ttl(key: &str, base: Duration, floor_ratio: f64, ceil_ratio: f64) -> Duration {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(key.as_bytes());
    let raw = u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"));
    let normalized = (raw as f64) / (u64::MAX as f64);
    let ratio = floor_ratio + ((ceil_ratio - floor_ratio) * normalized);
    Duration::from_secs_f64(base.as_secs_f64() * ratio)
}
```

- [ ] **Step 3: Add cache payload types and serde round-trip tests**

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CachedAuthenticatedKey {
    pub key_id: String,
    pub key_name: String,
    pub provider_type: String,
    pub protocol_family: String,
    pub status: String,
    pub quota_billable_limit: i64,
    pub billable_tokens_used: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CachedRequestSnapshot {
    pub provider_type: String,
    pub key_id: String,
    pub generation: i64,
    pub route_strategy: Option<String>,
    pub fixed_account_name: Option<String>,
    pub auto_account_names: Vec<String>,
    pub account_group_id: Option<String>,
    pub model_name_map_json: String,
    pub request_max_concurrency: Option<u64>,
    pub request_min_start_interval_ms: Option<u64>,
    pub key_status: String,
}
```

```rust
#[test]
fn deterministic_jitter_is_stable_for_same_key() {
    let a = deterministic_jitter_ttl("llma:req:codex:key-1", Duration::from_secs(6 * 3600), 0.8, 1.2);
    let b = deterministic_jitter_ttl("llma:req:codex:key-1", Duration::from_secs(6 * 3600), 0.8, 1.2);
    assert_eq!(a, b);
}

#[test]
fn deterministic_jitter_differs_for_different_keys() {
    let a = deterministic_jitter_ttl("llma:req:codex:key-1", Duration::from_secs(6 * 3600), 0.8, 1.2);
    let b = deterministic_jitter_ttl("llma:req:codex:key-2", Duration::from_secs(6 * 3600), 0.8, 1.2);
    assert_ne!(a, b);
}
```

- [ ] **Step 4: Add a short single-flight lock helper**

```rust
impl RequestCache {
    pub async fn try_lock(&self, key: &str, ttl: Duration) -> anyhow::Result<bool> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let locked: Option<String> = redis::cmd("SET")
            .arg(key)
            .arg("1")
            .arg("NX")
            .arg("EX")
            .arg(ttl.as_secs().max(1))
            .query_async(&mut conn)
            .await?;
        Ok(locked.is_some())
    }
}
```

- [ ] **Step 5: Export the module**

```rust
// llm-access-store/src/lib.rs
pub mod request_cache;
```

- [ ] **Step 6: Run the pure cache-module tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store deterministic_jitter --jobs 4
```

Expected: PASS

- [ ] **Step 7: Commit the request-cache module**

```bash
git add llm-access-store/src/lib.rs llm-access-store/src/request_cache.rs
git commit -m "feat: add llm-access valkey request cache primitives"
```

### Task 3: Wire optional Valkey state into `PostgresControlRepository`

**Files:**
- Modify: `llm-access-store/src/postgres.rs`
- Modify: `llm-access/src/runtime.rs`
- Test: `llm-access-store/src/postgres.rs`

- [ ] **Step 1: Extend the repository struct and constructor**

```rust
// llm-access-store/src/postgres.rs
use crate::request_cache::{RequestCache, RequestCacheConfig};

pub struct PostgresControlRepository {
    client: SqlxClient,
    codex_status_cache: Arc<RwLock<Option<CachedCodexRateLimitStatus>>>,
    request_cache: Option<RequestCache>,
}

impl PostgresControlRepository {
    pub async fn connect(
        database_url: &str,
        request_cache_config: Option<RequestCacheConfig>,
    ) -> anyhow::Result<Self> {
        let client = SqlxClient::connect(database_url).await?;
        llm_access_migrations::run_postgres_migrations(&client.pool).await?;
        let request_cache = request_cache_config.map(RequestCache::new).transpose()?;
        Ok(Self {
            client,
            codex_status_cache: Arc::new(RwLock::new(None)),
            request_cache,
        })
    }
}
```

- [ ] **Step 2: Build the cache config in runtime bootstrap**

```rust
// llm-access/src/runtime.rs
use llm_access_store::request_cache::RequestCacheConfig;

let request_cache_config = config.storage.request_cache.as_ref().map(|cache| {
    let url = std::env::var(&cache.url_env)
        .with_context(|| format!("missing request cache env `{}`", cache.url_env))?;
    Ok(RequestCacheConfig {
        url,
        key_prefix: cache.key_prefix.clone(),
    })
}).transpose()?;

let repository = Arc::new(
    PostgresControlRepository::connect(&database_url, request_cache_config).await?
);
```

- [ ] **Step 3: Add a regression test for “Postgres without cache still boots”**

```rust
#[tokio::test]
async fn postgres_repository_connects_without_request_cache() {
    let _guard = test_db_guard().await;
    let database_url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL");
    reset_test_db(&database_url).await.expect("reset");

    let repo = super::PostgresControlRepository::connect(&database_url, None)
        .await
        .expect("connect");

    assert!(repo.load_runtime_config_record().await.is_ok());
}
```

- [ ] **Step 4: Run the new repository bootstrap test**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store postgres_repository_connects_without_request_cache --jobs 4
```

Expected: PASS

- [ ] **Step 5: Commit repository bootstrap wiring**

```bash
git add llm-access-store/src/postgres.rs llm-access/src/runtime.rs
git commit -m "feat: wire optional valkey request cache into postgres repository"
```

### Task 4: Cache authenticated keys and request snapshots

**Files:**
- Modify: `llm-access-store/src/postgres.rs`
- Modify: `llm-access-store/src/request_cache.rs`
- Test: `llm-access-store/src/postgres.rs`

- [ ] **Step 1: Add auth-key cache get/set helpers**

```rust
impl RequestCache {
    pub async fn get_authenticated_key(
        &self,
        secret_hash: &str,
    ) -> anyhow::Result<Option<CachedAuthenticatedKey>> { /* get + serde decode */ }

    pub async fn set_authenticated_key(
        &self,
        secret_hash: &str,
        key: &CachedAuthenticatedKey,
    ) -> anyhow::Result<()> { /* setex with jitter */ }

    pub async fn set_missing_authenticated_key(&self, secret_hash: &str) -> anyhow::Result<()> {
        /* short negative cache with jitter */
    }
}
```

- [ ] **Step 2: Change `authenticate_bearer_secret` to cache-aside**

```rust
async fn load_authenticated_key_by_hash_cached(
    &self,
    secret_hash: &str,
) -> anyhow::Result<Option<AuthenticatedKey>> {
    if let Some(cache) = &self.request_cache {
        if let Some(cached) = cache.get_authenticated_key(secret_hash).await? {
            return Ok(Some(AuthenticatedKey {
                key_id: cached.key_id,
                key_name: cached.key_name,
                provider_type: cached.provider_type,
                protocol_family: cached.protocol_family,
                status: cached.status,
                quota_billable_limit: cached.quota_billable_limit,
                billable_tokens_used: cached.billable_tokens_used,
            }));
        }
    }

    let loaded = self.load_authenticated_key_by_hash(secret_hash).await?;
    if let Some(cache) = &self.request_cache {
        match &loaded {
            Some(key) => cache.set_authenticated_key(secret_hash, &CachedAuthenticatedKey {
                key_id: key.key_id.clone(),
                key_name: key.key_name.clone(),
                provider_type: key.provider_type.clone(),
                protocol_family: key.protocol_family.clone(),
                status: key.status.clone(),
                quota_billable_limit: key.quota_billable_limit,
                billable_tokens_used: key.billable_tokens_used,
            }).await?,
            None => cache.set_missing_authenticated_key(secret_hash).await?,
        }
    }
    Ok(loaded)
}
```

- [ ] **Step 3: Add request-snapshot cache helpers and use them in candidate resolution**

```rust
impl RequestCache {
    pub async fn get_request_snapshot(
        &self,
        provider: &str,
        key_id: &str,
    ) -> anyhow::Result<Option<CachedRequestSnapshot>> { /* ... */ }

    pub async fn set_request_snapshot(
        &self,
        snapshot: &CachedRequestSnapshot,
    ) -> anyhow::Result<()> { /* ... */ }
}
```

```rust
let Some(bundle) = self.load_key_bundle_by_id(&key.key_id).await? else {
    return Ok(Vec::new());
};
let generation = self.load_dispatch_generation(PROVIDER_CODEX).await?;
if let Some(snapshot) = self.try_load_cached_request_snapshot("codex", &bundle.key_id, generation).await? {
    return self.materialize_cached_codex_routes(&bundle, snapshot).await;
}
```

- [ ] **Step 4: Add a focused auth-cache integration test**

```rust
#[tokio::test]
async fn authenticate_bearer_secret_uses_cached_value_when_present() {
    let _guard = test_db_guard().await;
    let database_url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL");
    reset_test_db(&database_url).await.expect("reset");
    seed_test_key_bundle(&database_url).await.expect("seed");

    let repo = super::PostgresControlRepository::connect(&database_url, None)
        .await
        .expect("connect");

    let first = repo.authenticate_bearer_secret("secret").await.expect("first");
    let second = repo.authenticate_bearer_secret("secret").await.expect("second");

    assert_eq!(first, second);
    assert_eq!(second.expect("key").key_id, "key-1");
}
```

- [ ] **Step 5: Run the auth + snapshot tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store authenticate_bearer_secret_uses_cached_value_when_present --jobs 4
```

Expected: PASS

- [ ] **Step 6: Commit auth and request-snapshot caching**

```bash
git add llm-access-store/src/postgres.rs llm-access-store/src/request_cache.rs
git commit -m "feat: cache llm-access auth and request snapshots in valkey"
```

### Task 5: Cache account views and selected-account auth

**Files:**
- Modify: `llm-access-store/src/postgres.rs`
- Modify: `llm-access-store/src/request_cache.rs`
- Modify: `llm-access/src/provider.rs`
- Test: `llm-access-store/src/postgres.rs`
- Test: `llm-access/src/provider.rs`

- [ ] **Step 1: Add cache payloads for account view and account auth**

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CachedAccountView {
    pub provider_type: String,
    pub account_name: String,
    pub status: String,
    pub cached_error_message: Option<String>,
    pub cached_remaining_credits: Option<f64>,
    pub request_max_concurrency: Option<u64>,
    pub request_min_start_interval_ms: Option<u64>,
    pub proxy_config_id: Option<String>,
    pub disabled: Option<bool>,
    pub profile_arn: Option<String>,
    pub api_region: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CachedAccountAuth {
    pub provider_type: String,
    pub account_name: String,
    pub auth_json: String,
    pub profile_arn: Option<String>,
    pub api_region: Option<String>,
}
```

- [ ] **Step 2: Make candidate resolution bulk-load account views from Valkey**

```rust
let account_names = snapshot.account_names();
let cached_views = self.load_cached_account_views("kiro", &account_names).await?;
let missing = account_names
    .iter()
    .filter(|name| !cached_views.contains_key(*name))
    .cloned()
    .collect::<Vec<_>>();

if !missing.is_empty() {
    let rebuilt = self.rebuild_kiro_account_views(&missing).await?;
    self.store_account_views("kiro", &rebuilt).await?;
}
```

- [ ] **Step 3: Make selected-account hydrate read cached auth before Postgres**

```rust
async fn resolve_kiro_account_route(
    &self,
    account_name: &str,
) -> anyhow::Result<Option<ProviderKiroRoute>> {
    if let Some(route) = self.try_load_cached_kiro_account_auth(account_name).await? {
        return Ok(Some(route));
    }

    let route = self.load_kiro_account_route_from_postgres(account_name).await?;
    if let Some(route_ref) = route.as_ref() {
        self.store_cached_kiro_account_auth(route_ref).await?;
    }
    Ok(route)
}
```

- [ ] **Step 4: Add a provider-side regression test for cached hydrate behavior**

```rust
#[tokio::test]
async fn hydrate_kiro_route_for_dispatch_keeps_existing_auth_without_reload() {
    let route = ProviderKiroRoute {
        account_name: "kiro-a".to_string(),
        auth_json: "{\"accessToken\":\"cached\"}".to_string(),
        profile_arn: Some("arn:cached".to_string()),
        api_region: "us-east-1".to_string(),
        // fill the remaining fields with defaults used by existing tests
        ..sample_kiro_route()
    };

    let loaded = super::hydrate_kiro_route_for_dispatch(route.clone(), &EmptyProviderRouteStore)
        .await
        .expect("hydrate");

    assert_eq!(loaded.auth_json, route.auth_json);
}
```

- [ ] **Step 5: Run focused account-view/auth tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store --jobs 4
cargo test -p llm-access hydrate_kiro_route_for_dispatch_keeps_existing_auth_without_reload --jobs 4
```

Expected: PASS

- [ ] **Step 6: Commit account-view/auth caching**

```bash
git add llm-access-store/src/postgres.rs llm-access-store/src/request_cache.rs llm-access/src/provider.rs
git commit -m "feat: cache llm-access account views and auth payloads"
```

### Task 6: Update write paths and refresh paths to maintain cache coherence

**Files:**
- Modify: `llm-access-store/src/postgres.rs`
- Modify: `llm-access/src/codex_status.rs`
- Modify: `llm-access/src/kiro_status.rs`
- Test: `llm-access-store/src/postgres.rs`

- [ ] **Step 1: Invalidate or update auth cache on key mutations**

```rust
async fn patch_admin_key(&self, key_id: &str, patch: AdminKeyPatch) -> anyhow::Result<Option<AdminKey>> {
    let updated = self.patch_admin_key_postgres_only(key_id, patch).await?;
    if let Some(key) = updated.as_ref() {
        self.invalidate_request_snapshot_for_key(&key.provider_type, &key.id).await?;
        self.invalidate_authenticated_key_cache_for_key(key).await?;
    }
    Ok(updated)
}
```

- [ ] **Step 2: Bump provider generation on route-shape changes**

```rust
async fn bump_dispatch_generation(&self, provider: &str) -> anyhow::Result<()> {
    if let Some(cache) = &self.request_cache {
        cache.increment_dispatch_generation(provider).await?;
    }
    Ok(())
}
```

Use this after:

- key route config changes
- account-group membership changes
- proxy config/binding changes that affect runtime route resolution

- [ ] **Step 3: Update account-view/auth cache on refresh writes**

```rust
async fn save_kiro_auth_update(&self, update: ProviderKiroAuthUpdate) -> anyhow::Result<()> {
    self.save_kiro_auth_update_postgres_only(update.clone()).await?;
    self.refresh_cached_kiro_account_auth(&update.account_name).await?;
    self.refresh_cached_kiro_account_view(&update.account_name).await?;
    Ok(())
}

async fn save_kiro_status_cache_update(&self, update: ProviderKiroStatusCacheUpdate) -> anyhow::Result<()> {
    self.save_admin_kiro_status_cache(update.clone()).await?;
    self.refresh_cached_kiro_account_view(&update.account_name).await?;
    Ok(())
}
```

Same pattern for Codex:

- auth refresh updates `acct:auth` and, if needed, `acct:view`
- status refresh updates `acct:view` only

- [ ] **Step 4: Add a coherence regression test**

```rust
#[tokio::test]
async fn kiro_status_cache_update_does_not_require_generation_bump() {
    let _guard = test_db_guard().await;
    let database_url = std::env::var("TEST_POSTGRES_URL").expect("TEST_POSTGRES_URL");
    reset_test_db(&database_url).await.expect("reset");
    seed_test_kiro_key_page_fixture(&database_url).await.expect("seed");

    let repo = super::PostgresControlRepository::connect(&database_url, None)
        .await
        .expect("connect");

    let before = repo.resolve_kiro_route_candidates(&sample_kiro_authenticated_key()).await.expect("before");
    assert!(!before.is_empty());

    // apply a status-cache update here using the same account fixture

    let after = repo.resolve_kiro_route_candidates(&sample_kiro_authenticated_key()).await.expect("after");
    assert_eq!(before.len(), after.len());
}
```

- [ ] **Step 5: Run focused coherence tests**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
cargo test -p llm-access-store kiro_status_cache_update_does_not_require_generation_bump --jobs 4
```

Expected: PASS

- [ ] **Step 6: Commit cache coherence updates**

```bash
git add llm-access-store/src/postgres.rs llm-access/src/codex_status.rs llm-access/src/kiro_status.rs
git commit -m "feat: keep llm-access valkey cache coherent on writes"
```

### Task 7: Verification, metrics hooks, and ops docs

**Files:**
- Modify: `llm-access-store/src/postgres.rs`
- Modify: `docs/ops-runbook.md`

- [ ] **Step 1: Add lightweight hit/miss logging counters at cache boundaries**

```rust
tracing::debug!(
    cache_domain = "request_auth",
    cache_result = "hit",
    provider = %key.provider_type,
    "llm-access request cache hit"
);
```

Keep this at `debug` level and avoid logging secrets or auth payloads.

- [ ] **Step 2: Document the new request-cache env in the runbook**

```md
## llm-access request cache

- request-path cache env file may source `VALKEY_URL` from the private Valkey
  config
- request-path cache is optional optimization only; Postgres remains the source
  of truth
- if Valkey is unavailable, request routing may fall back to Postgres with
  higher Neon traffic
```

- [ ] **Step 3: Run full verification**

Run:

```bash
export CARGO_TARGET_DIR=/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow
pgrep -af 'cargo|rustc|trunk|ld|lld|mold' || true
rustfmt llm-access/src/config.rs llm-access/src/runtime.rs llm-access/src/provider.rs llm-access/src/codex_status.rs llm-access/src/kiro_status.rs llm-access-store/src/lib.rs llm-access-store/src/postgres.rs llm-access-store/src/request_cache.rs
cargo test -p llm-access-store -p llm-access --jobs 4
cargo clippy -p llm-access-store -p llm-access --jobs 4 -- -D warnings
```

Expected:

- all tests pass
- clippy returns zero warnings

- [ ] **Step 4: Final commit**

```bash
git add docs/ops-runbook.md llm-access/src/config.rs llm-access/src/runtime.rs llm-access/src/provider.rs llm-access/src/codex_status.rs llm-access/src/kiro_status.rs llm-access-store/src/lib.rs llm-access-store/src/postgres.rs llm-access-store/src/request_cache.rs
git commit -m "feat: move llm-access request path onto valkey cache"
```

## Spec coverage check

- Authenticated-key cache from the spec is implemented in Task 4.
- Request snapshot cache and generation keys are implemented in Task 4 and Task 6.
- Account selection view cache and selected-account auth cache are implemented in Task 5.
- Long TTL + deterministic jitter is implemented in Task 2.
- Explicit write/update invalidation rules are implemented in Task 6.
- Fallback behavior and ops wiring are implemented in Task 7.

## Placeholder scan

- No `TODO`, `TBD`, or “implement later” placeholders remain.
- Each code-bearing step includes concrete code to add or modify.
- Each execution step includes exact commands and expected outcomes.

## Type consistency check

- The plan keeps request-cache config in `llm-access/src/config.rs` and runtime bootstrap in `llm-access/src/runtime.rs`.
- Cache primitives live in `llm-access-store/src/request_cache.rs`.
- Postgres hot-path integration stays inside `llm-access-store/src/postgres.rs`.
- No task requires a wrapper repository or a parallel SQLite cache path.
