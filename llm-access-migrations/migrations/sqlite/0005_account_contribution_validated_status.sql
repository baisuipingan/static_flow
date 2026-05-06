ALTER TABLE llm_account_contribution_requests
RENAME TO llm_account_contribution_requests_old;

CREATE TABLE llm_account_contribution_requests (
    request_id TEXT PRIMARY KEY,
    account_name TEXT NOT NULL,
    account_id TEXT,
    id_token TEXT NOT NULL,
    access_token TEXT NOT NULL,
    refresh_token TEXT NOT NULL,
    requester_email TEXT NOT NULL,
    contributor_message TEXT NOT NULL,
    github_id TEXT,
    frontend_page_url TEXT,
    status TEXT NOT NULL CHECK (status IN ('pending', 'validated', 'issued', 'rejected', 'failed')),
    fingerprint TEXT NOT NULL,
    client_ip TEXT NOT NULL,
    ip_region TEXT NOT NULL,
    admin_note TEXT,
    failure_reason TEXT,
    imported_account_name TEXT,
    issued_key_id TEXT,
    issued_key_name TEXT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    processed_at_ms INTEGER CHECK (processed_at_ms IS NULL OR processed_at_ms >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO llm_account_contribution_requests (
    request_id, account_name, account_id, id_token, access_token, refresh_token,
    requester_email, contributor_message, github_id, frontend_page_url, status,
    fingerprint, client_ip, ip_region, admin_note, failure_reason,
    imported_account_name, issued_key_id, issued_key_name, created_at_ms,
    updated_at_ms, processed_at_ms
)
SELECT
    request_id, account_name, account_id, id_token, access_token, refresh_token,
    requester_email, contributor_message, github_id, frontend_page_url, status,
    fingerprint, client_ip, ip_region, admin_note, failure_reason,
    imported_account_name, issued_key_id, issued_key_name, created_at_ms,
    updated_at_ms, processed_at_ms
FROM llm_account_contribution_requests_old;

DROP TABLE llm_account_contribution_requests_old;

CREATE INDEX IF NOT EXISTS idx_llm_account_contribution_requests_status_created
    ON llm_account_contribution_requests(status, created_at_ms);
