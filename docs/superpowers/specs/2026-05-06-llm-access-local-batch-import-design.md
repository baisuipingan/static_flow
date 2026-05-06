# LLM Access Local Codex Batch Import Design

## Background

`llm-access` already supports single-account Codex import from the admin
accounts page. The current path accepts either sparse token fields or a raw
`auth_json` object, normalizes the payload, and inserts or updates one record
in `llm_codex_accounts`.

That is enough for manual one-off operations, but it is inefficient for local
bulk onboarding. The user wants the first batch-import version to work only
from local input, not from remote `CPA` or `sub2api` sources. The design
should therefore solve the batch execution model once, keep the input source
local and simple, and leave room for later source adapters without rewriting
the import core.

## Goals

- Add a local-only bulk import flow for Codex accounts in the existing admin
  `Codex Accounts` page.
- Use one pasted JSON array as the first input method.
- Reuse the current single-account auth normalization rules.
- Support optional refresh validation before import.
- Expose batch progress and per-item results in the admin UI.
- Prevent silent overwrite of existing accounts during batch import.
- Keep the design reusable for later local file upload and remote-source
  adapters.

## Non-Goals

- Do not add `CPA` browsing, `sub2api` browsing, or any other remote-source
  integration in this step.
- Do not add batch key issuance, account grouping, or contribution review logic
  to this flow.
- Do not auto-generate final account names for missing or conflicting names.
- Do not silently merge two different accounts based only on guessed identity.
- Do not introduce a second auth normalization path separate from the existing
  single-account import code.

## User Workflow

The first version adds a new bulk-import panel beside the current single-account
import form on the admin `Codex Accounts` page.

The operator pastes one JSON array. Each array element represents one intended
Codex account import item. The operator can optionally enable
`validate_before_import` before submitting the batch.

After submission, the backend creates an import job and returns immediately.
The frontend switches to a job-status card and polls for progress. The operator
can see:

- batch totals
- batch state
- per-item state
- per-item error messages
- final imported account name when successful

The first version does not allow editing individual rows inside the preview UI.
If the batch contains malformed or conflicting entries, the operator corrects
the source JSON and submits a new batch.

## Accepted Input Shape

The bulk-import request body sent by the frontend uses one top-level structure:

```json
{
  "provider_type": "codex",
  "source_type": "local_json",
  "validate_before_import": true,
  "items": [
    {
      "name": "acc-1",
      "auth_json": {
        "refresh_token": "rt-1",
        "account_id": "acct-1"
      }
    },
    {
      "name": "acc-2",
      "tokens": {
        "access_token": "at-2",
        "id_token": "id-2"
      }
    }
  ]
}
```

Rules:

- `provider_type` must be `codex` in this version.
- `source_type` must be `local_json` in this version.
- `items` must be a non-empty array.
- each item must provide `name`
- each item must provide either `auth_json` or `tokens`
- `auth_json` must be a JSON object when present
- `tokens` follows the existing sparse single-account token shape

The backend remains the source of truth for auth parsing and validation. The
frontend only performs shallow structural checks to avoid duplicated parsing
logic.

## Reused Import Semantics

The bulk flow reuses the same auth normalization already used by the current
single-account import endpoint:

- top-level snake_case token fields
- top-level camelCase token fields
- nested `tokens.*` snake_case fields
- nested `tokens.*` camelCase fields
- `access_token` or `refresh_token` is required

The reused normalization function is the existing single-account import path in
`llm-access/src/admin.rs`, not a new batch-specific parser.

## Backend API

### Create Job

`POST /admin/llm-gateway/accounts/import-jobs`

Request body:

- `provider_type`
- `source_type`
- `validate_before_import`
- `items`

Response body:

- job summary
- server-assigned `job_id`

The handler does not execute the whole batch inline. It validates the outer
shape, persists the job and items, starts background execution, and returns.

### List Jobs

`GET /admin/llm-gateway/accounts/import-jobs`

Response body:

- recent job summaries ordered newest first

This powers a lightweight “recent import jobs” area in the admin page.

### Get Job Detail

`GET /admin/llm-gateway/accounts/import-jobs/:job_id`

Response body:

- one job summary
- item list with current statuses and terminal error messages

The frontend polls this endpoint while the job is `pending` or `running`.

## Storage Model

### `llm_account_import_jobs`

One row per batch:

- `job_id TEXT PRIMARY KEY`
- `provider_type TEXT NOT NULL`
- `source_type TEXT NOT NULL`
- `validate_before_import INTEGER NOT NULL`
- `status TEXT NOT NULL`
- `total_count INTEGER NOT NULL`
- `completed_count INTEGER NOT NULL`
- `succeeded_count INTEGER NOT NULL`
- `skipped_count INTEGER NOT NULL`
- `failed_count INTEGER NOT NULL`
- `created_at_ms INTEGER NOT NULL`
- `updated_at_ms INTEGER NOT NULL`
- `finished_at_ms INTEGER`

Allowed `status` values:

- `pending`
- `running`
- `completed`
- `failed`

`completed` means the job finished processing all items even if some items
failed or conflicted. `failed` is reserved for batch-level failure, such as
internal startup failure before item execution can complete.

### `llm_account_import_job_items`

One row per batch item:

- `job_id TEXT NOT NULL`
- `item_index INTEGER NOT NULL`
- `requested_name TEXT NOT NULL`
- `requested_account_id TEXT`
- `raw_auth_json TEXT`
- `status TEXT NOT NULL`
- `error_message TEXT`
- `imported_account_name TEXT`
- `final_account_id TEXT`
- `validated_at_ms INTEGER`
- `imported_at_ms INTEGER`
- `created_at_ms INTEGER NOT NULL`
- `updated_at_ms INTEGER NOT NULL`

Primary key:

- `(job_id, item_index)`

Allowed item `status` values:

- `pending`
- `running`
- `imported`
- `skipped`
- `failed`
- `conflict`

`raw_auth_json` is temporary operational state. It exists to allow background
processing after request return, but it must be cleared once the item reaches a
terminal status. The terminal row keeps metadata and error output, not the
credential payload itself.

## Conflict Strategy

The first version is intentionally conservative.

### Existing Account Name

If `requested_name` already exists in `llm_codex_accounts`, mark the item as
`conflict` and do not overwrite the existing row.

This is stricter than the current single-account store behavior because the
underlying table upserts on `account_name`. Batch import must not silently turn
“paste many records” into “replace many existing accounts”.

### Existing Account Identity

If `requested_account_id` is present and already belongs to a different
existing account name, mark the item as `conflict`.

This keeps the first version simple and avoids ambiguous merge behavior.

### Duplicates Inside One Batch

If the same `requested_name` appears more than once in one submitted batch, the
first occurrence proceeds and later occurrences become `conflict`.

The implementation should detect this before attempting store writes so the
result is deterministic and easy to explain.

### Missing Account Identity

If `account_id` is absent, the batch flow does not try to infer cross-item
identity from token content. Name remains the only stable identity in that
case.

## Validation Semantics

### Without Validation

When `validate_before_import=false`, the backend:

1. normalizes auth input
2. performs conflict checks
3. inserts the account

This is the fast path for known-good local data.

### With Validation

When `validate_before_import=true`, each item must pass the same refresh-based
Codex validation flow already used for contribution approval:

1. normalize auth input
2. resolve the default Codex proxy binding
3. construct a temporary Codex route
4. call refresh-based validation
5. if refresh succeeds, import using the refreshed auth payload
6. if refresh fails, mark the item `failed`

Validation depends on a working default Codex proxy. If the default proxy is
missing or invalid, the item fails with that explicit reason. Other items in
the same job continue processing.

The batch does not have its own proxy override in version one. It always uses
the same default Codex proxy resolution as the existing validation path.

## Execution Model

The backend runs batch jobs asynchronously after the create-job request
returns.

The first version should use low parallelism:

- default worker concurrency: `1`

Reasoning:

- refresh validation is network-bound and sensitive to upstream rate limits
- deterministic item ordering simplifies debugging
- progress reporting is easier to reason about
- correctness matters more than throughput in the first release

Parallelism can be increased later behind one runtime constant after real usage
shows it is needed.

Per item, the worker performs:

1. mark item `running`
2. normalize auth
3. apply batch-local duplicate-name check
4. apply store conflict checks
5. optionally validate by refresh
6. create the account
7. clear `raw_auth_json`
8. write terminal status and summary fields
9. increment job counters

After the final item, the job becomes `completed` unless a job-level runtime
failure prevented further execution.

## Frontend Design

The current single-account import panel remains unchanged.

The page adds a second panel for “批量导入” with:

- one `validate_before_import` toggle
- one large JSON textarea
- one submit button
- one recent-jobs list
- one active job detail card

The textarea accepts one JSON array only. The UI does not support mixed manual
field editing per row in version one.

Submit-time frontend checks stay intentionally shallow:

- root must parse as JSON
- root must be an array
- each item must be an object
- each item must contain `name`
- each item must contain `auth_json` or `tokens`

The UI must not duplicate the backend’s full auth normalization behavior. It
may render helper text with one accepted example payload.

When a job is active:

- poll the detail endpoint
- show aggregate counters
- show per-item terminal rows
- stop polling when job enters a terminal state

The page does not need a separate route in version one. The existing
`admin_llm_gateway` page already owns account import UX and should remain the
single operational surface.

## Security And Data Handling

- raw auth payloads are accepted only on admin-authenticated endpoints
- batch job rows must not retain credential JSON after each item reaches a
  terminal state
- API responses must never echo token values back to the frontend
- server logs must report structural failures and item identifiers, not
  credential contents

This preserves the existing secret-handling expectations around auth import.

## Error Handling

Item-level errors should be explicit and stable enough for operators to act on:

- invalid JSON object
- missing `auth_json` or `tokens`
- missing `access_token` or `refresh_token`
- duplicate `requested_name` in the same batch
- existing account name conflict
- existing account identity conflict
- default Codex proxy missing
- default Codex proxy invalid
- refresh validation failure
- store write failure

A single bad item must not abort the whole job.

Job-level failure is reserved for cases such as:

- job row missing during execution
- repository unavailable before item loop can continue
- unrecoverable worker crash

## Testing

### Backend

- migration tests for the two new tables
- repository tests for job creation, item updates, and counter aggregation
- handler tests for create/list/detail endpoints
- batch-service tests covering:
  - valid batch without validation
  - valid batch with validation
  - duplicate names inside one batch
  - conflict against existing account name
  - conflict against existing `account_id`
  - validation failure on one item while other items continue
  - `raw_auth_json` cleared after terminal state

### Frontend

- parser tests for the JSON-array input gate
- request-shape tests for create-job payload
- component tests for job summary rendering and polling stop on terminal state

### Verification

Before implementation is considered complete:

- relevant Rust files formatted
- affected crates pass `cargo clippy ... -- -D warnings`
- affected backend tests pass
- affected frontend checks pass

## Rollout

The feature ships dark only in the sense that it lives on the authenticated
admin page. No public route changes are required.

The safe rollout order is:

1. land storage and backend job APIs
2. land frontend panel and polling UI
3. run local validation with small sample batches
4. manually verify conflict behavior against real existing accounts

## Follow-Up Work

This design intentionally leaves three follow-ups out of scope for version one:

- local multi-file `.json` upload as another source that maps into the same job
  pipeline
- `CPA` source adapter that converts remote auth-file browsing into batch
  import candidates
- `sub2api` source adapter that converts remote OAuth accounts into batch
  import candidates

Those features should reuse the same job tables, item statuses, validation
rules, and import core rather than introducing new execution paths.
