# llm-access single-core primary and edge-secondary design

Date: 2026-05-17
Status: first version implemented

## Summary

Keep `llm-access` horizontally scalable on the API side while making the
mutable usage and background-refresh responsibilities explicitly single-primary.

The first version introduces two node classes:

- one `core` node class that mounts JuiceFS and is eligible to become the
  single cluster primary;
- any number of `edge` nodes that do not mount JuiceFS and never participate in
  primary election.

At runtime:

- all nodes serve normal `llm-access` API traffic;
- all nodes write compact local usage journals;
- only the elected `core` primary runs background refresh, consumes journals
  into DuckDB, writes JuiceFS-backed usage artifacts, and serves as the durable
  usage-query truth source;
- `edge` workers proxy usage/admin history queries to the current primary
  worker and relay sealed local journal files to that primary for ingestion.

Version one keeps cluster discovery and machine metadata in the shared
`llma:*` Valkey namespace. One configured `core` node advertises itself as the
single primary, while `edge` nodes consume that primary snapshot and proxy
usage responsibilities accordingly. Proxy metadata cache uses the same
namespace.

This design intentionally avoids multi-core automatic failover in version one.
If the single `core` primary is unavailable, `edge` nodes continue serving API
traffic and continue buffering local usage journals, while usage/history views
degrade explicitly.

## Relation to earlier designs

This design supersedes the previously drafted federated multi-node usage model
in `2026-05-16-llm-access-neon-control-and-federated-usage-design.md`.

The earlier design assumed every node could own its own immutable usage
namespace and serve its own local usage query truth. The current direction is
more constrained and simpler:

- one primary worker owns the shared usage truth;
- secondary nodes proxy usage views instead of owning independent usage
  archives;
- JuiceFS is required only on the primary-eligible `core` class.

## Problem

The current production split already has the right broad shape:

- the API writes compact local journals instead of writing DuckDB directly;
- the usage worker consumes journals and writes tiered DuckDB plus detail packs;
- the request path uses Postgres plus shared Valkey;
- usage/admin history routes already proxy through `usage_query_base_url`.

But the current implementation still assumes a single machine:

1. Background refresh starts locally on every API process unless disabled.
2. The usage worker consumes one local journal root into one local/shared usage
   namespace.
3. Journal consumer state is keyed by local `file_sequence`, which is not
   enough for multi-node relay.
4. Proxy resolution metadata still leaks into Postgres hot reads through:
   - `SELECT ... FROM llm_proxy_configs ORDER BY ...`
   - `SELECT ... FROM llm_proxy_bindings WHERE provider_type = $1`
5. The frontend has no notion of whether a usage view is local primary data or
   a secondary node's proxied view.

The user explicitly wants:

- only one primary node to perform refresh and usage ingestion;
- non-primary nodes to stay stateless except for local journal buffering;
- primary eligibility to depend on actual mounted capability, especially
  JuiceFS;
- node role to be discovered automatically from shared cluster state;
- node and primary metadata to live mostly in Redis/Valkey rather than being
  repeatedly read from Postgres;
- admin and usage pages to expose whether the current node is primary or is
  proxying to the primary;
- the missing proxy metadata cache work to be folded into the same project.

## Goals

- allow multiple `llm-access` API nodes to serve normal request traffic;
- keep usage-journal production local on every node;
- enforce exactly one primary node for:
  - Codex/Kiro background refresh;
  - usage journal ingestion;
  - DuckDB and JuiceFS writes;
  - usage query truth;
- require JuiceFS only on primary-eligible `core` nodes;
- allow `edge` nodes to answer usage/admin history requests by proxying to the
  current primary worker;
- make node role and usage-data source visible in admin/frontend responses;
- move proxy metadata reads into the shared Valkey cache namespace;
- keep the first rollout simple: one configured `core` primary plus many
  `edge` nodes, all coordinated through shared Valkey metadata.

## Non-goals

- no version-one support for multiple `core` nodes automatically failing over
  among themselves;
- no version-one support for a node without JuiceFS becoming primary;
- no multi-writer DuckDB or multi-writer JuiceFS usage namespace;
- no shared journal root across machines;
- no distributed worker-consumer lease across many writers targeting one shared
  journal directory;
- no attempt to preserve usage/history availability when the single primary is
  down;
- no redesign of the request-path Valkey cache namespace beyond extending it.

## Current state

Current production responsibilities are already split as follows:

- API process:
  - provider traffic;
  - control-plane reads/writes through Postgres;
  - request-path Valkey cache reads/writes;
  - local usage journal production;
  - optional background refresh;
- usage worker:
  - local sealed-journal consumption;
  - active DuckDB writes;
  - archived segment publication;
  - detail-pack writes;
  - worker-side usage query routes.

Verified storage layout:

- control/state mount: `/mnt/llm-access`
- usage analytics mount: `/mnt/llm-access-usage`
- local usage journal: `/var/lib/staticflow/llm-access/usage-journal`
- local active DuckDB: `/var/lib/staticflow/llm-access/analytics-active`

Important code assumptions today:

- background refresh is started during API startup:
  `llm-access/src/lib.rs:474`
- API usage history routes proxy through `usage_query_base_url`:
  `llm-access/src/public.rs:538`
- the worker claims sealed journal files locally by rename:
  `llm-access/src/usage_worker.rs:212`
- the journal consumer state treats `file_sequence` as the consumed-file
  identity:
  `llm-usage-journal/src/state.rs:39`
- request-path Valkey cache already exists under `llma:*`, but does not yet
  cache proxy configs or proxy bindings:
  `llm-access-store/src/request_cache.rs`

## Chosen approach

Use a **single-core primary plus any-number-of-edge-secondaries** topology.

### Why this approach

This is preferred over the main alternatives for four reasons:

1. It preserves the current local-journal model instead of trying to invent a
   shared distributed journal.
2. It keeps API scale-out independent from JuiceFS mounts and DuckDB writes.
3. It makes the mutable state boundary explicit: one node writes usage truth,
   all others proxy or buffer.
4. It solves the user's machine-awareness requirement without pretending that
   secondaries own their own authoritative usage history.

### Rejected alternatives

#### 1. All nodes primary-eligible

Rejected because nodes without JuiceFS cannot safely ingest or publish usage
truth. Letting them join leader election would create invalid states.

#### 2. All nodes become federated usage owners

Rejected because that returns to the more complex earlier federated design and
reintroduces node-specific immutable usage namespaces and merged multi-node
queries. The user has since narrowed the desired shape: secondary workers
should forward to one primary worker.

#### 3. Multi-core automatic failover in version one

Rejected because the user explicitly narrowed the first rollout to one `core`
primary only. Introducing lease promotion and standby failover now would add
complexity without helping the immediate deployment target.

## 1. Node classes

Each deployment node has a configured node class:

- `core`
- `edge`

This is local configuration, not dynamically inferred from arbitrary host
state. The local config answers:

- is this node allowed to mount JuiceFS?
- is this node eligible to become primary?

Version one requires exactly one deployed `core` node in normal production
topology. More may exist later, but automatic multi-core failover is out of
scope.

## 2. Runtime roles

Runtime role is discovered automatically from shared Valkey cluster state:

- `primary`
- `edge-secondary`
- `degraded`

Rules:

- a configured `core` node always resolves to `primary` in version one;
- an `edge` node never tries to acquire primary;
- an `edge` node becomes `edge-secondary` when a valid primary snapshot is
  known;
- an `edge` node becomes `degraded` when no valid primary snapshot is known.

The user requirement "if no primary exists, the first node should default to
primary" is implemented as:

- if the deployment has one configured `core` node, that node publishes itself
  as primary;
- if an `edge` node sees no primary snapshot, it cannot self-promote and must
  degrade explicitly.

## 3. Valkey cluster truth and read view

Version one keeps cluster discovery in Valkey because there is only one
supported `core` node. This avoids inventing a lease protocol before
multi-core failover is actually needed.

All cluster metadata lives in the existing `llma:*` namespace.

Recommended keys:

- `llma:cluster:primary`
- `llma:cluster:node:<node_id>`

Suggested payloads:

### `llma:cluster:primary`

- `node_id`
- `api_base_url`
- `worker_base_url`
- `published_at_ms`

### `llma:cluster:node:<node_id>`

- `node_id`
- `node_class`
- `runtime_role`
- `display_name`
- `region`
- `api_base_url`
- `worker_base_url`
- `primary_node_id`
- `usage_query_mode` (`local_primary` or `proxied_primary`)
- `last_heartbeat_at_ms`

### TTL policy

These are short-lived heartbeat-style cache entries, not multi-hour dispatch
cache entries.

Recommended defaults:

- node snapshot TTL: `20-30s`
- primary snapshot TTL: `15s`
- heartbeat update cadence: around `5s`

No read path should rely on these keys remaining available forever. If the keys
are absent, an `edge` node falls back to local degraded logic.

Operational constraints for version one:

- exactly one `core` node may be deployed at a time;
- that `core` node must mount JuiceFS;
- `edge` nodes must not mount or write the shared usage namespace;
- later Postgres-backed lease truth, standby core nodes, and failover are a
  separate iteration.

## 4. Primary responsibilities

The primary node has all of these responsibilities:

- run Codex and Kiro background refresh loops;
- serve as the usage-query truth source;
- consume local and relayed journal files;
- write active DuckDB, archived segments, segment catalog, and detail packs;
- publish node and primary metadata snapshots into Valkey.

Only the primary mounts and writes the usage analytics JuiceFS paths in the
supported production topology.

## 6. Edge-secondary responsibilities

An `edge-secondary` node:

- serves normal API traffic;
- writes its own local usage journal;
- proxies usage/history/admin worker queries to the primary worker;
- relays sealed local journal files to the primary worker for ingestion;
- updates its own node snapshot in Valkey.

It does **not**:

- run background refresh;
- ingest journal files into DuckDB locally;
- write usage archives, catalog, or detail packs to JuiceFS;
- participate in leader election.

## 7. Usage journal flow

### API producer

All nodes keep the current producer behavior:

1. persist control-plane rollups through Postgres;
2. append compact diagnostic usage events into the local journal root;
3. seal journal files by age and size thresholds.

This preserves the local write path and avoids synchronous dependence on the
primary worker during request handling.

### Primary worker

The primary worker continues to:

- claim sealed files from its journal root;
- ingest events into DuckDB;
- mark files consumed;
- delete imported local files;
- run retention maintenance.

### Secondary worker relay

A secondary worker handles its own local sealed files differently:

1. discover the current primary worker from Valkey;
2. claim the oldest local sealed file;
3. send the sealed file to the primary worker's internal ingest endpoint;
4. wait for primary ack;
5. only then mark the file consumed locally and delete it.

If the relay fails, the local file must return to the sealed queue and be
retried later. The secondary node must not drop a sealed file solely because
the primary is temporarily unavailable.

## 8. Relay ingest idempotency

Current local consumer state only keys consumption by `file_sequence`, which is
root-local and insufficient for multi-node relay.

Version one must extend primary ingest idempotency to use a compound identity:

- `source_node_id`
- `file_sequence`
- `file_digest`

The primary ingest ledger may store additional fields such as:

- `event_count`
- `imported_at_ms`

If the same sealed file is relayed twice, the second ingest must be accepted as
an idempotent duplicate and must not append duplicate events into DuckDB.

## 9. Query and forwarding model

### Query source of truth

The primary worker is the only authoritative usage/history query source in
version one.

### Local request path

API nodes keep using one local `usage_query_base_url`.

- on the primary node, that points to the local primary worker;
- on an edge node, that points to the local secondary worker.

The secondary worker then proxies usage/history requests to the primary worker.

This keeps the API-side contract stable while moving primary-awareness into the
worker layer.

### Proxy semantics

The secondary worker must forward at least:

- public usage lookup requests;
- admin usage event list/detail requests;
- worker status requests that the UI relies on for current usage/history views.

The response must include metadata that lets the frontend distinguish:

- local primary data;
- proxied primary data;
- degraded/no-primary state.

## 10. Frontend and admin UX

Usage-related admin surfaces must become node-aware.

### Page-level state banner

At minimum, `/admin/llm-access` and the usage event pages should show:

- current node id;
- current node class;
- current runtime role;
- current primary node id;
- usage source:
  - `local_primary`
  - `proxied_primary`
  - `primary_unavailable`

### Worker and journal indicators

Show:

- current worker query mode.

### Response metadata

To minimize payload churn, version one should prefer response headers for
transport metadata, for example:

- `x-llm-access-node-id`
- `x-llm-access-worker-role`
- `x-llm-access-primary-node-id`
- `x-llm-access-usage-source`

The frontend can render the machine-awareness state from those headers and
supplement it with the worker-status JSON.

## 11. Proxy metadata cache extension

This project also folds in the missing proxy metadata request-cache coverage.

The current hot-read leak comes from:

- loading all proxy configs from `llm_proxy_configs`;
- loading one provider binding from `llm_proxy_bindings`;
- rebuilding proxy-resolution context from Postgres on cache misses.

Version one adds these shared Valkey keys in the same `llma:*` namespace:

- `llma:proxy:configs`
- `llma:proxy:binding:codex`
- `llma:proxy:binding:kiro`

### Read path

Proxy-resolution context should:

1. read proxy configs from `llma:proxy:configs`;
2. read the provider-specific binding key;
3. fall back to Postgres only on cache miss or decode failure.

### Write path invalidation

Invalidate and optionally repopulate these keys after:

- create proxy config;
- patch proxy config;
- delete proxy config;
- update proxy binding;
- import legacy Kiro proxy configs.

This keeps the previously observed `llm_proxy_configs` and
`llm_proxy_bindings` queries out of the steady-state request path.

## 12. Configuration

Each node needs explicit local configuration for identity and class.

Recommended env vars:

- `LLM_ACCESS_NODE_ID`
- `LLM_ACCESS_NODE_CLASS` (`core` or `edge`)
- `LLM_ACCESS_NODE_DISPLAY_NAME`
- `LLM_ACCESS_NODE_REGION`
- `LLM_ACCESS_NODE_API_BASE_URL`
- `LLM_ACCESS_NODE_WORKER_BASE_URL`

### Core node requirements

Core nodes must mount:

- `/mnt/llm-access`
- `/mnt/llm-access-usage`

### Edge node requirements

Edge nodes do not mount JuiceFS in the supported topology.

Therefore, any configuration that is currently only available through JuiceFS
must be moved to node-local secret/env management before edge rollout. That
includes shared runtime secrets that the API process still needs even when the
node is not primary.

## 13. Failure handling

### Primary unavailable

If the primary is unavailable:

- edge APIs continue serving normal request traffic;
- edge APIs continue writing local journals;
- secondary workers stop proxying usage/history successfully;
- frontend/admin usage views must show explicit degraded state.

Version one does not silently invent another truth source.

### Secondary relay backlog growth

If the primary is unavailable long enough:

- edge sealed-file backlog grows locally;
- backlog is visible in node metadata and admin UI;
- no usage data is dropped solely because the primary is temporarily down.

### Valkey unavailable

If Valkey is unavailable:

- an edge node cannot discover a primary and degrades;
- request-path proxy metadata falls back to Postgres reads;
- performance degrades, but correctness remains.

### Postgres unavailable

If Postgres is unavailable:

- control-plane truth is unavailable;
- services should fail closed for control-plane operations;
- existing local journal buffering does not become a substitute control plane.

## 14. Rollout plan

Recommended rollout order:

1. add cluster node-class config and Valkey-published primary snapshot;
2. gate background refresh behind runtime role;
3. add worker query proxy mode for edge nodes;
4. add primary relay ingest endpoint and compound idempotency ledger;
5. switch secondary workers to sealed-file relay mode;
6. add frontend/admin machine-awareness banner and metadata rendering;
7. add proxy metadata Valkey cache keys and invalidation hooks;
8. deploy one core primary and one edge secondary as the first live topology.

## 15. Verification

Implementation must prove all of these before rollout is considered complete:

- the configured `core` node publishes itself as primary automatically at
  startup;
- an `edge` node never self-promotes to primary;
- only the primary runs background refresh;
- edge API requests still succeed while edge worker proxies usage/history to the
  primary worker;
- edge sealed files are relayed and ingested exactly once;
- primary ingest is idempotent for duplicate relays of the same source file;
- frontend/admin usage pages clearly indicate local vs proxied vs unavailable
  usage source;
- the observed `llm_proxy_configs` and `llm_proxy_bindings` query growth is
  removed from the steady-state request path.

## 16. Explicit version-one boundaries

Version one is intentionally narrow:

- one deployed `core` primary;
- no automatic multi-core failover;
- `edge` nodes do not mount JuiceFS;
- usage/history truth remains centralized in the primary worker;
- local journal buffering preserves request-path availability even when usage
  views degrade.

That scope is deliberate. It satisfies the user's current scaling target
without pretending the system is already a full distributed multi-writer
cluster.
