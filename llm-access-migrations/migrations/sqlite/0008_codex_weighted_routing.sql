ALTER TABLE llm_runtime_config
    ADD COLUMN codex_weight_free INTEGER NOT NULL DEFAULT 1 CHECK (codex_weight_free >= 0);

ALTER TABLE llm_runtime_config
    ADD COLUMN codex_weight_plus INTEGER NOT NULL DEFAULT 10 CHECK (codex_weight_plus >= 0);

ALTER TABLE llm_runtime_config
    ADD COLUMN codex_weight_pro5x INTEGER NOT NULL DEFAULT 50 CHECK (codex_weight_pro5x >= 0);

ALTER TABLE llm_runtime_config
    ADD COLUMN codex_weight_pro20x INTEGER NOT NULL DEFAULT 200 CHECK (codex_weight_pro20x >= 0);
