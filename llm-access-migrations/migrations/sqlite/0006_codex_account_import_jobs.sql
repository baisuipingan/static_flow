CREATE TABLE IF NOT EXISTS llm_account_import_jobs (
    job_id TEXT PRIMARY KEY,
    provider_type TEXT NOT NULL CHECK (provider_type IN ('codex')),
    source_type TEXT NOT NULL CHECK (source_type IN ('local_json')),
    validate_before_import INTEGER NOT NULL CHECK (validate_before_import IN (0, 1)),
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    total_count INTEGER NOT NULL CHECK (total_count >= 0),
    completed_count INTEGER NOT NULL CHECK (completed_count >= 0),
    succeeded_count INTEGER NOT NULL CHECK (succeeded_count >= 0),
    skipped_count INTEGER NOT NULL CHECK (skipped_count >= 0),
    failed_count INTEGER NOT NULL CHECK (failed_count >= 0),
    batch_error_message TEXT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    finished_at_ms INTEGER CHECK (finished_at_ms IS NULL OR finished_at_ms >= 0)
) STRICT, WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_llm_account_import_jobs_created
    ON llm_account_import_jobs(created_at_ms DESC);

CREATE TABLE IF NOT EXISTS llm_account_import_job_items (
    job_id TEXT NOT NULL REFERENCES llm_account_import_jobs(job_id) ON DELETE CASCADE,
    item_index INTEGER NOT NULL CHECK (item_index >= 0),
    requested_name TEXT NOT NULL,
    requested_account_id TEXT,
    raw_auth_json TEXT CHECK (raw_auth_json IS NULL OR json_valid(raw_auth_json)),
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'imported', 'skipped', 'failed', 'conflict')),
    error_message TEXT,
    imported_account_name TEXT,
    final_account_id TEXT,
    validated_at_ms INTEGER CHECK (validated_at_ms IS NULL OR validated_at_ms >= 0),
    imported_at_ms INTEGER CHECK (imported_at_ms IS NULL OR imported_at_ms >= 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (job_id, item_index)
) STRICT, WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_llm_account_import_job_items_job_status
    ON llm_account_import_job_items(job_id, status, item_index);
