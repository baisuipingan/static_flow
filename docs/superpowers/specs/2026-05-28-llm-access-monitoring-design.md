# LLM Access Monitoring Design

## Goal

Add an admin monitoring page for recent llm-access operational metrics without
changing existing usage-event API serialization. Proxy attribution must be
resolved during usage-worker consumption, cached in Valkey, and persisted into
analytics rows so monitoring queries never need control-plane joins.

## Constraints

- Keep existing `/admin/llm-gateway/usage*` and public usage JSON unchanged.
- Do not reintroduce SQLite runtime catalog paths.
- Tiered/archive queries must minimize segment openings and avoid turning
  Postgres into a second event-detail store.
- Monitoring must work on recent windows (`15m`, `1h`, `6h`, `24h`) and return
  hotspot queries within a few seconds after warmup.

## Architecture

### 1. Consumption-time proxy attribution

The usage worker gets a concrete `PostgresControlRepository` handle in addition
to the existing runtime-config store handle. During journal consumption it
deduplicates `(provider_type, account_name)` pairs, resolves the effective proxy
 metadata once per account, and writes that attribution into the analytics fact
rows before persisting them.

The attribution payload contains:

- `proxy_source_at_event`
- `proxy_config_id_at_event`
- `proxy_config_name_at_event`
- `proxy_url_at_event`

Resolution is cached in Valkey with provider dispatch generation so admin proxy
or account changes invalidate future lookups naturally.

### 2. Analytics storage and query path

The DuckDB `usage_events` fact table stores the new proxy-attribution columns.
Existing usage list/detail serialization stays unchanged because those handlers
do not project the new columns.

Monitoring queries use a new dedicated metrics path. They scan only the columns
needed for operational aggregates, prune archive segments by the existing PG
catalog time window, and open each matching segment at most once per request.
The query path computes:

- overall summary
- top first-token-latency accounts and proxies
- non-OK request distributions by account and proxy
- routing-wait hot spots by account and proxy
- quota-failover hot spots by account and proxy
- downstream-disconnect hot spots by account and proxy
- non-OK status-code distribution

### 3. HTTP/API surface

Add a new worker endpoint and matching llm-access API proxy route:

- `/admin/llm-gateway/usage/metrics`

The endpoint accepts recent-window presets plus optional provider/source
filters. It returns a dedicated metrics JSON schema separate from the existing
usage list/detail schema.

### 4. Frontend

Add a dedicated admin route and page:

- `/admin/llm-gateway/monitor`

The page shows summary cards, window/provider controls, and metric tables for
accounts and proxies. It polls on a short interval, but uses the new dedicated
metrics endpoint rather than reusing usage pagination APIs.

## Testing

- Unit/integration coverage for worker-side proxy attribution caching.
- DuckDB metrics query coverage on hot and tiered/archive data.
- Router/API tests for the new metrics proxy route.
- Frontend type-check/build coverage via the normal workspace build and
  self-hosted frontend build.
