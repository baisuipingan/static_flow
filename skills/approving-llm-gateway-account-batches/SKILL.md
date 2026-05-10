---
name: approving-llm-gateway-account-batches
description: Use when pending LLM Gateway account-contribution requests need to be validated, issued, and patched in bulk through the admin API, including deterministic proxy assignment and per-account concurrency settings.
---

# Approving LLM Gateway Account Batches

Use this skill after public account-contribution requests are already queued as
`pending`. It automates the admin side:

1. fetch pending requests,
2. validate each request,
3. approve and issue each account,
4. patch the imported account with proxy and request settings.

This skill does not bypass the existing review flow. It only drives the
existing admin API in batch form.

## When To Use

- The public batch submit step already created `pending` requests.
- You want to process one batch end to end through admin APIs.
- Imported Codex accounts should receive standard settings after issue.

## Admin API Assumptions

- Admin base examples:
  - `http://127.0.0.1:19182`
  - `http://127.0.0.1:19082`
- Required routes:
  - `GET /admin/llm-gateway/account-contribution-requests`
  - `POST /admin/llm-gateway/account-contribution-requests/{id}/validate`
  - `POST /admin/llm-gateway/account-contribution-requests/{id}/approve-and-issue`
  - `PATCH /admin/llm-gateway/accounts/{name}`
  - `GET /admin/llm-gateway/accounts`
  - `GET /admin/llm-gateway/proxy-configs`

## Preferred Workflow

1. Confirm the batch prefix and expected request count.
2. Run a dry run first.
3. Process only matching `pending` requests.
4. Validate before issue for every request.
5. Patch imported accounts immediately after issue.
6. Verify all matching requests moved out of `pending`.

## Proxy Assignment Rule

- Only consider `active` proxy configs.
- Count current usage from `active` Codex accounts with `proxy_config_id`.
- Choose the proxy with the lowest current count.
- Break ties by proxy name for deterministic output.
- Update the in-memory count after each assignment so one batch is balanced.

## Standard Patch Settings

- `request_max_concurrency = 3`
- `request_min_start_interval_ms = random[100, 1000]`
- `proxy_mode = fixed`
- `proxy_config_id = selected least-used proxy`

## Helper Script

Run:

```bash
python3 skills/approving-llm-gateway-account-batches/scripts/approve_account_contribution_batch.py --help
```

Dry run:

```bash
python3 skills/approving-llm-gateway-account-batches/scripts/approve_account_contribution_batch.py \
  --admin-base-url "http://127.0.0.1:19182" \
  --account-prefix "pickup7_" \
  --expected-count 20 \
  --admin-note "batch validate and issue"
```

Real run:

```bash
python3 skills/approving-llm-gateway-account-batches/scripts/approve_account_contribution_batch.py \
  --admin-base-url "http://127.0.0.1:19182" \
  --account-prefix "pickup7_" \
  --expected-count 20 \
  --admin-note "batch validate and issue" \
  --apply
```

## Notes

- Prefer local pb-mapper admin access on `127.0.0.1:19182` when available.
- The script stops neither validation nor issue routing logic inside the
  backend; it only sequences admin calls.
- The script records one JSON result file under `/tmp/`.
