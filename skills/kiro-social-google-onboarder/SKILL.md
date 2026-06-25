---
name: kiro-social-google-onboarder
description: Automate Kiro CLI account onboarding with social Google login through the required HTTP proxy, local Kiro SQLite auth cleanup without logout, llm-access Kiro account import, proxy assignment, balance refresh, and KIRO STUDENT/1000-credit verification. Use when adding a new Google-backed Kiro account, replacing a mistakenly imported AWS/IDC Kiro login, or verifying that a local Kiro social account is student-tier before exposing it through StaticFlow/llm-access.
---

# Kiro Social Google Onboarder

## Boundaries

- Never run `kiro-cli logout`; remove local metadata keys instead.
- Never store Google passwords, access tokens, refresh tokens, or raw token JSON in the skill or handoff.
- Use the HTTP proxy for the whole login flow. Default proxy: `http://127.0.0.1:11111`.
- Delete llm-access accounts only by exact explicit name, and only when the user asked to replace or clean that account.
- Keep the final verification centered on `kiro-cli whoami`, `auth_method=social`, `provider=google`, and refreshed Kiro balance.

## One-Command Flow

Use the bundled script for the standard path:

```bash
read -rsp 'Google password: ' KIRO_GOOGLE_PASSWORD; echo
export KIRO_GOOGLE_PASSWORD
python3 skills/kiro-social-google-onboarder/scripts/onboard_kiro_social_google.py \
  --email user@example.com \
  --account-name kiro-user-google-social \
  --replace-account
unset KIRO_GOOGLE_PASSWORD
```

The script:

1. Backs up `~/.local/share/kiro-cli/data.sqlite3`.
2. Deletes only Kiro auth metadata keys from local SQLite.
3. Starts Kiro social Google device authorization through the proxy.
4. Opens an isolated Chrome profile through the proxy and drives the visible Google/Kiro pages through DevTools.
   It must inspect the DOM and click real `button`/`a`/`role=button` controls such as `Next`, `Approve`, `Continue`, and `Restart`.
5. Approves the Kiro device code and polls the social token endpoint.
6. Writes `kirocli:social:token` and `api.codewhisperer.profile` locally.
7. Verifies `kiro-cli whoami` through the proxy.
8. Runs `kiro-local-account-importer` dry-run and apply against `http://127.0.0.1:19182`.
   Proxy assignment is delegated to the importer, which chooses the least-used
   active United States proxy first and uses latency only as a tie-breaker.
9. Refreshes balance and fails unless the account is `KIRO STUDENT` with at least `1000` usage limit by default.
10. Removes temporary browser profiles and token response files.

## Required Options

- `--email`: Google account email.
- `--account-name`: llm-access Kiro account name. Prefer `kiro-<localpart>-google-social`.
- Password source: `KIRO_GOOGLE_PASSWORD` by default, or interactive hidden prompt.

## Useful Options

- `--proxy http://127.0.0.1:11111`: override the login proxy.
- `--admin-base-url http://127.0.0.1:19182`: override local mapped llm-access admin API.
- `--replace-account`: delete the exact target account before importing.
- `--delete-account-name NAME`: delete an exact mistakenly imported account before the flow.
- `--manual-timeout-seconds 300`: time allowed for manual CAPTCHA/MFA completion if Google requires it.
- `--expect-usage-limit 1000`: required usage limit for verification.
- `--no-expect-student`: allow non-student accounts, but still print the balance.

## Failure Handling

- If Google shows CAPTCHA or MFA, complete it in the launched isolated browser; the script keeps polling until the manual timeout.
- If Google shows `Something went wrong` with `Restart`, click `Restart` and continue the same OAuth flow.
- If the import step fails after account creation, query the exact account name before retrying. Do not create a second account name unless the user requests it.
- If balance is not `KIRO STUDENT` or usage limit is below `1000`, report the account as not acceptable and do not hide the failure.
- If a wrong AWS/IDC account was imported, delete that exact account via llm-access admin API, then rerun the social flow. Do not call logout.

## Verification Commands

```bash
HTTP_PROXY=http://127.0.0.1:11111 \
HTTPS_PROXY=http://127.0.0.1:11111 \
ALL_PROXY=http://127.0.0.1:11111 \
kiro-cli whoami

curl -fsS -X POST \
  http://127.0.0.1:19182/admin/kiro-gateway/accounts/<account-name>/balance \
  | jq '{subscription_title, usage_limit, remaining, current_usage, user_id}'
```
