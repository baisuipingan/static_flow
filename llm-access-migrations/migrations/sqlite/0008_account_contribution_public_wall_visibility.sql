ALTER TABLE llm_account_contribution_requests
ADD COLUMN show_on_public_wall INTEGER NOT NULL DEFAULT 1 CHECK (
    show_on_public_wall IN (0, 1)
);
