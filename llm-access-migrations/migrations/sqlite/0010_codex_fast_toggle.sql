ALTER TABLE llm_key_route_config
ADD COLUMN codex_fast_enabled INTEGER NOT NULL DEFAULT 1 CHECK (
    codex_fast_enabled IN (0, 1)
);
