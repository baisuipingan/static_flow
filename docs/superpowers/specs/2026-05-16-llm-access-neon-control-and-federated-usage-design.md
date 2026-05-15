# llm-access Neon control plane and federated multi-node usage design

Date: 2026-05-16
Status: approved in brainstorming, pending implementation plan

## Summary

Move the `llm-access` control plane from the current shared SQLite file to Neon
Postgres, keep the existing SQLite file on disk as rollback-only state, and
stop treating it as the live source of truth after cutover.

At the same time, formalize a multi-node usage design that does not rely on a
central usage query node and does not let multiple workers write into one
shared archive/catalog namespace. Each node keeps a local hot
`usage-journal` and a local active DuckDB segment, writes immutable usage
artifacts only into its own JuiceFS namespace, and exposes node-local query
endpoints. API/UI usage inspection becomes federated: users can target one
machine directly for real-time state, or request a global view that fans out
across nodes and merges results.

This design intentionally separates:

- durable control-plane truth: Neon Postgres;
- disposable but queryable usage analytics: per-node local journal plus
  per-node JuiceFS namespaces;
- real-time machine observability: on-demand forwarding to the selected node,
  not high-frequency state writes into Postgres.

## Problem

The current production design has two independent issues:

1. Control-plane state is still anchored to a shared SQLite file under
   `/mnt/llm-access/control/llm-access.sqlite3`, which is not a safe long-term
   design for multi-node concurrent writes.
2. Usage data paths were originally designed around single-node assumptions.
   The current worker model expects one journal root, one consumer-state DB,
   and one tiered archive/catalog namespace. That shape does not extend
   cleanly to multiple nodes that each generate and inspect usage events.

The user has explicitly accepted these operational boundaries:

- the Neon snapshot imported on 2026-05-16 can be treated as the new control
  truth at cutover time;
- any recent SQLite-side quota drift can be discarded;
- the old SQLite file must remain on disk, but it does not need to stay live;
- `usage-journal` must not consume object-storage bandwidth and therefore must
  stay on local disk;
- users need machine-aware usage inspection, including node-scoped RPM and
  real-time journal status views.

## Goals

- replace live control-plane reads/writes with Neon Postgres;
- keep the old SQLite control file for rollback and audit only;
- remove any remaining live dependence on a shared SQLite file for
  multi-node-safe control writes;
- ensure `usage-journal` is fully local-disk-based on every node;
- support multiple nodes that each generate usage events without sharing one
  journal root or one archive/catalog writer;
- allow users to inspect usage by machine and to switch between nodes from the
  frontend;
- allow a global usage view without introducing a dedicated central usage query
  node;
- keep Postgres writes for node liveness minimal and bounded in cost.

## Non-goals

- no attempt to preserve exact post-import SQLite quota deltas before cutover;
- no dual-write migration phase between SQLite and Postgres;
- no attempt to make multiple nodes share one journal root;
- no attempt to make multiple nodes concurrently publish into one shared
  archive/catalog namespace;
- no centralized usage query service;
- no append-only node-heartbeat history in Postgres;
- no deletion of the existing SQLite control file.

## Current state

Current production paths and ownership:

- live control DB: `/mnt/llm-access/control/llm-access.sqlite3`
- hot local usage journal: `/var/lib/staticflow/llm-access/usage-journal`
- local active DuckDB: `/var/lib/staticflow/llm-access/analytics-active`
- archived immutable usage segments:
  `/mnt/llm-access-usage/analytics/segments`
- usage catalog:
  `/mnt/llm-access-usage/analytics/catalog`
- detail packs:
  `/mnt/llm-access-usage/details/packs/...`

Current code assumptions that block naive multi-node scaling:

- one journal root owns one `writer-state.sqlite3` and one
  `consumer-state.sqlite3`;
- journal file sequence allocation is root-local and monotonic;
- the worker claims sealed files by renaming `sealed/...` into
  `consuming/...`;
- the tiered catalog is a single shared SQLite file
  `usage-segments.sqlite3`;
- usage queries are proxied to one local worker base URL, currently
  `http://127.0.0.1:19081`.

These assumptions are valid in single-node mode and should not be stretched
into a pseudo-distributed design.

## Chosen approach

### Control plane

Move the live control plane to Neon Postgres with a single primary database and
no dual-write overlap.

The Neon snapshot imported on 2026-05-16 becomes the initial canonical
dataset. At cutover time, API and worker binaries switch from opening the
SQLite control repository to opening a Postgres control repository. The
existing SQLite file remains untouched on JuiceFS as a rollback fallback and a
historical reference.

### Usage architecture

Use a federated multi-node design:

- every node owns its own local journal root;
- every node runs its own local usage ingest worker;
- every node writes immutable usage artifacts only into its own node-specific
  JuiceFS namespace;
- every node serves local usage query endpoints for its own data;
- API and frontend become node-aware and can forward usage inspection requests
  to a selected node;
- global usage inspection fans out to multiple nodes and merges results in the
  requesting API node.

### Node discovery

Use a static node registry in Postgres plus low-frequency row updates for
coarse liveness.

Real-time machine state is not continuously pushed into Postgres. Instead, it
is fetched on demand from the selected node.

## Why this approach

This design is preferred over the main alternatives for these reasons:

1. It removes shared-SQLite control writes, which are the real multi-node
   correctness problem.
2. It keeps `usage-journal` local, so the hottest usage write path never hits
   JuiceFS or object storage.
3. It avoids a central usage query node, which the user explicitly does not
   want.
4. It avoids a shared multi-writer archive/catalog namespace, which would be
   fragile and expensive to reason about.
5. It keeps Postgres traffic low by storing only stable node metadata plus
   coarse liveness fields, while real-time observability remains pull-based.

## Architecture

### 1. Live control plane in Neon Postgres

The current SQLite-backed control store is replaced by a Postgres-backed
repository implementing the same store traits currently served by
`SqliteControlRepository`.

The first migration scope is full replacement of the existing control-plane
tables, not a partial split. The live repository must own all current
control-plane reads and writes that currently land in SQLite, including:

- admin runtime config;
- keys and key route config;
- key usage rollups;
- account groups;
- proxy configs and proxy bindings;
- Codex accounts and status cache;
- Kiro accounts and status cache;
- token requests;
- account contribution requests;
- sponsor requests;
- import jobs and import job items;
- schema migration bookkeeping.

There is no live split-brain design. After cutover, Postgres is the only
authoritative control store.

### 2. Per-node local journal and active segment

Each node keeps:

- local usage journal root:
  `/var/lib/staticflow/llm-access/usage-journal`
- local active mutable DuckDB root:
  `/var/lib/staticflow/llm-access/analytics-active`

These remain node-local and must not be placed on JuiceFS.

The old historical path `/mnt/llm-access/usage-journal` becomes dead data.
It should be cleaned up and must not stay in live service configuration.

### 3. Per-node immutable usage namespace on JuiceFS

Each node writes immutable worker-owned artifacts under a dedicated node
namespace, for example:

- `/mnt/llm-access-usage/nodes/<node_id>/analytics/segments/...`
- `/mnt/llm-access-usage/nodes/<node_id>/analytics/catalog/...`
- `/mnt/llm-access-usage/nodes/<node_id>/details/packs/...`

`<node_id>` is a stable configured identifier, not a derived hostname. It must
not change across reboots or ordinary machine maintenance.

One node writes only its own namespace. No node writes another node's archive,
catalog, or details subtree.

### 4. Federated query and forwarding

Each node exposes local usage-query routes for its own usage state. API nodes
become node-aware and support two query modes:

- node-scoped query:
  - user selects one node;
  - API forwards to that node's usage-query endpoint;
  - response includes node identity and is not silently re-routed elsewhere.
- global-scoped query:
  - API resolves the enabled node set from Postgres;
  - API fans out requests to all eligible nodes;
  - API merges and sorts the results;
  - response includes partial-failure metadata when one or more nodes are
    unavailable.

This design provides machine-aware observability without introducing a central
usage-query service.

## Configuration and secret management

### Neon connection configuration

Create a dedicated control-plane config file on the shared control JuiceFS
mount:

- `/mnt/llm-access/config/neon.env`

Recommended contents:

- `LLM_ACCESS_CONTROL_DATABASE_URL=postgresql://...`
- optional future split settings such as statement timeouts or SSL options if
  needed

This file is the durable source of the Postgres connection URL for both API and
usage-worker services.

The systemd env file under `/etc/llm-access/llm-access.env` should source or
explicitly inject the values from this shared config so rollout remains
configuration-driven rather than code-driven.

The connection URL should be treated as long-lived but rotatable operational
configuration. Password, role, branch, or endpoint changes must be handled by
updating this config file, not by recompiling binaries.

### Node identity configuration

Each node needs explicit local identity configuration, for example:

- `LLM_ACCESS_NODE_ID=<stable-id>`
- `LLM_ACCESS_NODE_DISPLAY_NAME=<human-readable-name>`
- `LLM_ACCESS_NODE_QUERY_BASE_URL=http://<node-local-or-routable-address>:19081`

These values should be operationally managed and must not be inferred from
ephemeral hostnames.

### Node registration model

Nodes are explicitly provisioned in Postgres by operators. Runtime node
processes do not auto-create registry rows.

First-version behavior:

- operators create or update the node row;
- runtime processes only update liveness/status fields on their own existing
  row;
- unknown `node_id` values fail startup or fail heartbeat registration rather
  than silently self-registering.

## Postgres data model

### Rehosted control tables

The existing SQLite control schema is re-expressed in Postgres with equivalent
semantic ownership. JSON-bearing `TEXT` columns become `jsonb` where practical.
Boolean-like integer flags become `boolean`.

The live schema does not need to remain bit-for-bit SQLite-compatible. It needs
to preserve application semantics and data ownership.

### New node-registry table

Add a stable node registry table in Postgres, for example `llm_usage_nodes`,
with fields equivalent to:

- `node_id` primary key
- `display_name`
- `query_base_url`
- `write_namespace_prefix`
- `enabled`
- `roles_json`
- `last_seen_at_ms`
- `last_probe_status`
- `last_error_summary`
- `last_capability_version`
- `created_at_ms`
- `updated_at_ms`

Important constraints:

- one row per node;
- no append-only heartbeat history table;
- liveness updates are row updates only;
- this table stores discovery and routing metadata, not high-frequency
  observability samples.

## Liveness and observability model

### Coarse liveness in Postgres

Each node updates its own registry row at low frequency, with a default target
interval of 60 seconds, by updating:

- `last_seen_at_ms`
- `last_probe_status`
- `last_error_summary` when useful

This is intentionally one-row overwrite behavior, not append-only history.

### Real-time state on demand

Real-time machine state must be pulled from the target node when a user opens a
machine-scoped view.

Examples of node-local real-time data:

- current RPM / request rate;
- current worker state;
- journal active file sequence;
- sealed file count and bytes;
- consuming file count;
- bad file count;
- local worker heartbeat and current import progress;
- local active DuckDB size;
- recent local query latency or error state if exposed.

These must not be continuously mirrored into Postgres.

## Frontend and admin UX

The admin usage surfaces become machine-aware.

### Node selector

Usage views must show a node selector with:

- `All nodes`
- one entry per enabled node from the registry

Each entry should show at least:

- display name;
- coarse online/offline state;
- last seen age;
- node role badges when relevant.

### Node-scoped usage views

When a user picks a node, the UI should show node-local real-time state,
including:

- current RPM for that node;
- current journal status for that node;
- current worker status/progress for that node;
- usage events produced by that node;
- details lookups resolved from that node's namespace.

### Global usage views

When a user selects `All nodes`, the UI should show:

- merged usage event list;
- aggregated totals;
- per-node response status so partial outages are visible;
- an explicit indicator when the results are partial because one or more nodes
  failed to respond.

The UI must not hide node-level failures behind a fake success state.

## Usage data ownership and layout

### Local paths

Per node:

- journal root:
  `/var/lib/staticflow/llm-access/usage-journal`
- active mutable DuckDB:
  `/var/lib/staticflow/llm-access/analytics-active`

### Shared per-node namespaces on JuiceFS

Per node:

- archive segments:
  `/mnt/llm-access-usage/nodes/<node_id>/analytics/segments/YYYY/MM/DD/...`
- segment catalog:
  `/mnt/llm-access-usage/nodes/<node_id>/analytics/catalog/...`
- detail packs:
  `/mnt/llm-access-usage/nodes/<node_id>/details/packs/<provider>/YYYY/MM/DD/...`

This preserves the existing low-cost cleanup principle:

- cleanup decisions are metadata-driven;
- details and archived `.duckdb` contents are not read just to decide
  retention;
- one node never cleans another node's local journal;
- shared cleanup operates only within the node's own namespace.

## Query semantics

### Node-scoped

- exact node selected by user;
- API forwards to the requested node;
- if node is unavailable, return node-unavailable error;
- do not silently fall back to another node.

### Global-scoped

- API reads node registry from Postgres;
- API sends concurrent requests to all enabled nodes;
- API merges event pages and sorts by event time and event identity;
- response includes per-node status metadata;
- partial failures are explicit.

Pagination for global mode must be designed carefully because naive
per-node offset-based pagination can produce unstable views. The implementation
plan must define one consistent merge-pagination contract before coding starts.

## Cutover plan

### Preconditions

- Neon Postgres already contains the imported control snapshot;
- this imported snapshot is accepted as the new starting truth;
- SQLite control remains intact on JuiceFS;
- a short control-plane maintenance window is acceptable.

### Cutover shape

1. Freeze or minimize admin/control mutations for a short window.
2. Roll out code that can read the Neon config from shared config path.
3. Switch API and worker control repositories from SQLite to Postgres.
4. Restart services in a controlled order.
5. Verify control-plane reads, writes, and auth/routing behavior.
6. Re-enable normal admin/control operations.

There is no backfill replay from SQLite after cutover.

### SQLite after cutover

The old SQLite file:

- stays on disk;
- is not deleted;
- is not live-written anymore;
- is kept only for rollback and historical inspection.

## Rollback plan

Rollback is configuration-driven:

1. stop or drain affected services;
2. point service configuration back to SQLite control path;
3. restart services;
4. verify control-plane behavior;
5. leave Postgres data intact for later inspection.

Rollback does not require deleting Postgres data.

## Failure handling

### Node unavailable during node-scoped view

- return explicit node-unavailable response;
- preserve target node identity in the error payload;
- do not substitute another node.

### Node unavailable during global view

- return partial results with per-node errors;
- show which nodes responded and which did not;
- do not treat partial fan-out as a full success.

### Postgres unavailable

- control-plane operations fail closed;
- services should surface clear control-store unavailability;
- SQLite rollback remains the operational escape hatch.

### Node namespace corruption

- node-owned usage data remains disposable;
- worker cleanup may delete corrupted usage artifacts within that node
  namespace;
- control-plane state in Postgres is unaffected.

## Cost controls

This design intentionally bounds unnecessary spend:

- no journal writes to JuiceFS;
- no append-only heartbeat history in Postgres;
- no high-frequency RPM or file-list writes to Postgres;
- node registry writes are coarse row updates only;
- real-time machine state is pulled on demand;
- immutable usage artifacts are partitioned by node namespace, avoiding shared
  writer contention and repair traffic.

## Operational rules

- `usage-journal` must stay on local disk on every node;
- node identity must be explicit and stable;
- one node writes only its own immutable usage namespace;
- Postgres control config lives under shared control config path on JuiceFS;
- API and worker must read the same control-plane database URL;
- the old SQLite control file must not be deleted during or after cutover.

## Implementation phases

1. Add Postgres control-store implementation and runtime selection.
2. Add shared Postgres config loading from `/mnt/llm-access/config/neon.env`.
3. Add node identity configuration and Postgres node registry support.
4. Move any lingering live `usage-journal` config off JuiceFS paths.
5. Introduce per-node shared usage namespace layout.
6. Add node-aware forwarding and federated global query behavior.
7. Add admin/frontend node selector and node-scoped observability panels.
8. Cut over live control from SQLite to Postgres.

## Verification expectations

Before rollout is considered complete, implementation must prove:

- API and usage worker both read live control from Postgres;
- SQLite file remains present and untouched as rollback state;
- no live service still points `usage-journal` to a JuiceFS path;
- one node cannot publish into another node's shared usage namespace;
- node-scoped queries forward correctly;
- global queries return merged results with partial-failure metadata;
- frontend usage/admin surfaces expose machine-aware inspection;
- Postgres node liveness writes remain bounded and low-frequency.

## Explicit rejected alternatives

### Central usage-query node

Rejected because the user explicitly wants nodes to be independently
inspectable and forwardable without introducing a central query authority.

### Shared multi-writer archive/catalog namespace

Rejected because the current tiered usage catalog and publish path are not a
true distributed multi-writer design. Pushing multiple nodes into one shared
catalog would add contention and correctness risk for little value.

### Append-only heartbeat table in Postgres

Rejected because it adds storage churn and compute cost while duplicating
information that can be fetched live from the target node.

### Shared journal root with file prefixes

Rejected because the journal state model is rooted in one local
`writer-state.sqlite3` plus one local `consumer-state.sqlite3`. File naming
alone does not convert that model into a sound distributed queue.

## Final design statement

The live `llm-access` control plane moves fully to Neon Postgres, seeded from
the already imported dataset and cut over without dual-write overlap. SQLite is
retained only as rollback state.

Usage remains node-local at the hot write path, node-namespaced on JuiceFS at
the immutable artifact layer, and federated at read time. Machine-aware
observability is implemented through explicit node selection and on-demand
forwarding, not through a central query node or high-frequency state writes
into Postgres.
