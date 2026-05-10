#!/usr/bin/env python3
import argparse
import json
import random
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def api_request(base_url: str, method: str, path: str, payload=None):
    url = f"{base_url.rstrip('/')}{path}"
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            raw = response.read()
            if not raw:
                return None
            return json.loads(raw)
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"{method} {path} failed: HTTP {exc.code} {body}") from exc
    except urllib.error.URLError as exc:
        raise RuntimeError(f"{method} {path} failed: {exc}") from exc


def choose_proxy(proxy_rows, proxy_counts):
    ordered = sorted(
        proxy_rows,
        key=lambda row: (proxy_counts.get(row["id"], 0), row["name"]),
    )
    return ordered[0]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--admin-base-url", required=True)
    parser.add_argument("--account-prefix", required=True)
    parser.add_argument("--expected-count", type=int, required=True)
    parser.add_argument("--admin-note", default="batch validate and issue")
    parser.add_argument("--request-max-concurrency", type=int, default=3)
    parser.add_argument("--interval-min", type=int, default=100)
    parser.add_argument("--interval-max", type=int, default=1000)
    parser.add_argument("--seed", type=int)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()

    if args.expected_count <= 0:
        raise SystemExit("expected-count must be positive")
    if args.interval_min < 0 or args.interval_max < args.interval_min:
        raise SystemExit("invalid interval range")

    rng = random.Random(args.seed)

    requests_payload = api_request(
        args.admin_base_url,
        "GET",
        "/admin/llm-gateway/account-contribution-requests?status=pending&limit=500",
    )
    pending = [
        row
        for row in requests_payload["requests"]
        if row["status"] == "pending" and row["account_name"].startswith(args.account_prefix)
    ]
    pending.sort(key=lambda row: row["account_name"])

    if len(pending) != args.expected_count:
        raise SystemExit(
            f"expected {args.expected_count} pending requests for prefix {args.account_prefix}, "
            f"found {len(pending)}"
        )

    accounts_payload = api_request(
        args.admin_base_url,
        "GET",
        "/admin/llm-gateway/accounts?provider_type=codex&limit=500",
    )
    proxy_payload = api_request(
        args.admin_base_url,
        "GET",
        "/admin/llm-gateway/proxy-configs",
    )

    proxy_rows = [row for row in proxy_payload["proxy_configs"] if row["status"] == "active"]
    if not proxy_rows:
        raise SystemExit("no active proxy configs found")

    proxy_counts = {}
    for account in accounts_payload["accounts"]:
        if account.get("status") != "active":
            continue
        proxy_id = account.get("proxy_config_id")
        if not proxy_id:
            continue
        proxy_counts[proxy_id] = proxy_counts.get(proxy_id, 0) + 1

    timestamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    result_path = Path(f"/tmp/llm-gateway-account-batch-approve-{timestamp}.json")

    plan = []
    local_counts = dict(proxy_counts)
    for row in pending:
        proxy = choose_proxy(proxy_rows, local_counts)
        interval_ms = rng.randint(args.interval_min, args.interval_max)
        plan.append(
            {
                "request_id": row["request_id"],
                "account_name": row["account_name"],
                "selected_proxy_id": proxy["id"],
                "selected_proxy_name": proxy["name"],
                "request_max_concurrency": args.request_max_concurrency,
                "request_min_start_interval_ms": interval_ms,
            }
        )
        local_counts[proxy["id"]] = local_counts.get(proxy["id"], 0) + 1

    if not args.apply:
        result_path.write_text(
            json.dumps(
                {
                    "mode": "dry-run",
                    "admin_base_url": args.admin_base_url,
                    "account_prefix": args.account_prefix,
                    "expected_count": args.expected_count,
                    "plan": plan,
                },
                ensure_ascii=False,
                indent=2,
            )
        )
        print(result_path)
        print(json.dumps({"mode": "dry-run", "planned": len(plan)}, ensure_ascii=False))
        return

    results = []
    failures = []
    applied_counts = dict(proxy_counts)

    for item in plan:
        result = dict(item)
        try:
            validated = api_request(
                args.admin_base_url,
                "POST",
                f"/admin/llm-gateway/account-contribution-requests/{urllib.parse.quote(item['request_id'], safe='')}/validate",
                {"admin_note": args.admin_note},
            )
            issued = api_request(
                args.admin_base_url,
                "POST",
                f"/admin/llm-gateway/account-contribution-requests/{urllib.parse.quote(item['request_id'], safe='')}/approve-and-issue",
                {"admin_note": args.admin_note},
            )
            imported_name = issued.get("imported_account_name") or item["account_name"]
            patched = api_request(
                args.admin_base_url,
                "PATCH",
                f"/admin/llm-gateway/accounts/{urllib.parse.quote(imported_name, safe='')}",
                {
                    "proxy_mode": "fixed",
                    "proxy_config_id": item["selected_proxy_id"],
                    "request_max_concurrency": item["request_max_concurrency"],
                    "request_min_start_interval_ms": item["request_min_start_interval_ms"],
                },
            )
            applied_counts[item["selected_proxy_id"]] = (
                applied_counts.get(item["selected_proxy_id"], 0) + 1
            )
            result.update(
                {
                    "validated_status": validated.get("status"),
                    "issued_status": issued.get("status"),
                    "imported_account_name": imported_name,
                    "issued_key_id": issued.get("issued_key_id"),
                    "issued_key_name": issued.get("issued_key_name"),
                    "patched_proxy_mode": patched.get("proxy_mode"),
                    "patched_proxy_config_id": patched.get("proxy_config_id"),
                    "patched_request_max_concurrency": patched.get("request_max_concurrency"),
                    "patched_request_min_start_interval_ms": patched.get(
                        "request_min_start_interval_ms"
                    ),
                }
            )
            results.append(result)
        except Exception as exc:  # noqa: BLE001
            result["error"] = str(exc)
            failures.append(result)
            results.append(result)

    result_path.write_text(
        json.dumps(
            {
                "mode": "apply",
                "admin_base_url": args.admin_base_url,
                "account_prefix": args.account_prefix,
                "expected_count": args.expected_count,
                "results": results,
                "failure_count": len(failures),
            },
            ensure_ascii=False,
            indent=2,
        )
    )
    print(result_path)
    print(
        json.dumps(
            {
                "mode": "apply",
                "processed": len(results),
                "failures": len(failures),
            },
            ensure_ascii=False,
        )
    )
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
