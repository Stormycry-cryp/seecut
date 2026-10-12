PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    email_verified_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS email_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose IN ('verify_email', 'reset_password')),
    token_hash TEXT NOT NULL UNIQUE,
    expires_at INTEGER NOT NULL,
    failed_attempts INTEGER NOT NULL DEFAULT 0,
    consumed_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS teams (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    owner_user_id TEXT NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS team_members (
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    joined_at INTEGER NOT NULL,
    PRIMARY KEY (team_id, user_id)
);

CREATE TABLE IF NOT EXISTS invite_links (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    creator_user_id TEXT NOT NULL REFERENCES users(id),
    token_hash TEXT NOT NULL UNIQUE,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER,
    consumed_by_user_id TEXT REFERENCES users(id),
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS uploads (
    id TEXT PRIMARY KEY,
    owner_user_id TEXT NOT NULL REFERENCES users(id),
    team_id TEXT REFERENCES teams(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose IN ('team_asset', 'generation_input')),
    object_key TEXT NOT NULL UNIQUE,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    expected_size INTEGER NOT NULL,
    sha256 TEXT,
    expires_at INTEGER NOT NULL,
    completed_at INTEGER,
    write_token TEXT,
    media_kind TEXT CHECK (media_kind IN ('image', 'video', 'audio')),
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS team_assets (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    uploader_user_id TEXT NOT NULL REFERENCES users(id),
    upload_id TEXT NOT NULL UNIQUE REFERENCES uploads(id),
    object_key TEXT NOT NULL UNIQUE,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    sha256 TEXT,
    source_task_id TEXT,
    deleted_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_team_assets_team_created
    ON team_assets(team_id, created_at DESC);

CREATE TABLE IF NOT EXISTS wallets (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    available_credits INTEGER NOT NULL DEFAULT 0 CHECK (available_credits >= 0),
    held_credits INTEGER NOT NULL DEFAULT 0 CHECK (held_credits >= 0),
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS wallet_holds (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    task_id TEXT NOT NULL UNIQUE,
    credits INTEGER NOT NULL CHECK (credits >= 0),
    state TEXT NOT NULL CHECK (state IN ('held', 'captured', 'released')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ledger_entries (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    kind TEXT NOT NULL CHECK (kind IN ('purchase', 'hold', 'capture', 'release', 'adjustment')),
    delta_available INTEGER NOT NULL,
    delta_held INTEGER NOT NULL,
    reference_type TEXT NOT NULL,
    reference_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS plans (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    price_fen INTEGER NOT NULL CHECK (price_fen > 0),
    credits INTEGER NOT NULL CHECK (credits > 0),
    active INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS orders (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    plan_id TEXT NOT NULL REFERENCES plans(id),
    provider TEXT NOT NULL CHECK (provider IN ('alipay')),
    amount_fen INTEGER NOT NULL,
    credits INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('created', 'pending', 'paid', 'closed', 'failed')),
    provider_trade_no TEXT,
    paid_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER
);

-- Purchase intents use the same account-scoped digest/target contract as Agent
-- requests. Legacy orders have no intent key and an unknown expiry (NULL).
CREATE TABLE IF NOT EXISTS order_idempotency (
    user_id TEXT NOT NULL REFERENCES users(id),
    key TEXT NOT NULL,
    digest TEXT NOT NULL,
    order_id TEXT NOT NULL REFERENCES orders(id),
    PRIMARY KEY(user_id, key)
);

CREATE TABLE IF NOT EXISTS payment_events (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    provider_event_id TEXT NOT NULL,
    order_id TEXT NOT NULL REFERENCES orders(id),
    verified INTEGER NOT NULL,
    payload_sha256 TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(provider, provider_event_id)
);

CREATE TABLE IF NOT EXISTS generation_tasks (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    kind TEXT NOT NULL CHECK (kind IN ('image', 'video')),
    provider TEXT NOT NULL DEFAULT 'xiangxin' CHECK (provider IN ('image2', 'xiangxin')),
    operation TEXT NOT NULL DEFAULT 'generate' CHECK (operation IN ('generate', 'edit')),
    model TEXT NOT NULL,
    prompt TEXT NOT NULL,
    request_json TEXT NOT NULL,
    batch_id TEXT,
    batch_index INTEGER NOT NULL DEFAULT 0,
    batch_size INTEGER NOT NULL DEFAULT 1,
    trashed_at INTEGER,
    idempotency_key TEXT NOT NULL,
    quoted_credits INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'submitting', 'provider_accepted', 'processing', 'validating', 'succeeded', 'failed', 'pending_reconcile', 'expired')),
    upstream_task_id TEXT,
    upstream_response_json TEXT,
    error_code TEXT,
    error_message TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    next_attempt_at INTEGER NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    UNIQUE(user_id, idempotency_key)
);

CREATE TABLE IF NOT EXISTS generation_quotes (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    model TEXT NOT NULL,
    canonical_json TEXT NOT NULL,
    effort_policy_fingerprint TEXT,
    billing_key TEXT NOT NULL,
    credits INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS generation_assets (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    upload_id TEXT NOT NULL UNIQUE REFERENCES uploads(id),
    provider_asset_id TEXT,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS generation_outputs (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES generation_tasks(id) ON DELETE CASCADE,
    object_key TEXT NOT NULL UNIQUE,
    content_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

-- Agent uses the existing wallet; a turn budget is a ceiling, never a debit.
CREATE TABLE IF NOT EXISTS assistant_turn_budgets (
 id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id),
 local_session_id TEXT NOT NULL, turn_id TEXT NOT NULL,
 model_id TEXT NOT NULL DEFAULT 'default',
 effort_id TEXT, policy_revision TEXT, policy_json TEXT, route_fingerprint TEXT,
 max_credits INTEGER NOT NULL CHECK(max_credits > 0),
 held_credits INTEGER NOT NULL DEFAULT 0 CHECK(held_credits >= 0),
 spent_credits INTEGER NOT NULL DEFAULT 0 CHECK(spent_credits >= 0),
 rounding_mode TEXT NOT NULL DEFAULT 'legacy_operation' CHECK(rounding_mode IN ('legacy_operation','task_cumulative_v1')),
 accrued_microcredits INTEGER NOT NULL DEFAULT 0 CHECK(accrued_microcredits >= 0),
 operation_count INTEGER NOT NULL DEFAULT 0, max_steps INTEGER NOT NULL,
 state TEXT NOT NULL DEFAULT 'active', created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
 UNIQUE(user_id,local_session_id,turn_id)
);
CREATE TABLE IF NOT EXISTS assistant_idempotency (
 user_id TEXT NOT NULL REFERENCES users(id), namespace TEXT NOT NULL,
 key TEXT NOT NULL, digest TEXT NOT NULL, target_id TEXT NOT NULL,
 PRIMARY KEY(user_id,namespace,key)
);
CREATE TABLE IF NOT EXISTS assistant_quotes (
 id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id),
 request_digest TEXT NOT NULL, pricing_version TEXT NOT NULL,
 input_token_upper_bound INTEGER NOT NULL, payload_digest TEXT NOT NULL,
 route_fingerprint TEXT NOT NULL, input_rate INTEGER NOT NULL, output_rate INTEGER NOT NULL, context_window INTEGER NOT NULL,
 rate_denominator INTEGER NOT NULL DEFAULT 1000 CHECK(rate_denominator IN (1000,1000000)),
 max_credits INTEGER NOT NULL, expires_at INTEGER NOT NULL,
 consumed_at INTEGER, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS assistant_operations (
 id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id),
 client_request_id TEXT NOT NULL, local_session_id TEXT NOT NULL,
 turn_id TEXT NOT NULL, step_id TEXT NOT NULL,
 budget_id TEXT NOT NULL REFERENCES assistant_turn_budgets(id),
 request_digest TEXT NOT NULL, request_json TEXT NOT NULL,
 input_token_upper_bound INTEGER NOT NULL, payload_digest TEXT NOT NULL,
 pricing_version TEXT NOT NULL, input_rate INTEGER NOT NULL, output_rate INTEGER NOT NULL,
 rate_denominator INTEGER NOT NULL DEFAULT 1000 CHECK(rate_denominator IN (1000,1000000)), route_fingerprint TEXT NOT NULL, context_window INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','dispatching','running','succeeded','failed','cancelled','outcome_unknown')),
 version INTEGER NOT NULL DEFAULT 1, last_event_seq INTEGER NOT NULL DEFAULT 0,
 cancel_requested INTEGER NOT NULL DEFAULT 0,
 billing_state TEXT NOT NULL DEFAULT 'held', reserved_credits INTEGER NOT NULL,
 captured_credits INTEGER NOT NULL DEFAULT 0, released_credits INTEGER NOT NULL DEFAULT 0,
 result_json TEXT, error_json TEXT, usage_json TEXT,
 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
 UNIQUE(user_id,client_request_id), UNIQUE(user_id,local_session_id,turn_id,step_id)
);
CREATE TABLE IF NOT EXISTS assistant_events (
 operation_id TEXT NOT NULL REFERENCES assistant_operations(id), seq INTEGER NOT NULL,
 type TEXT NOT NULL, payload_json TEXT NOT NULL, created_at INTEGER NOT NULL,
 PRIMARY KEY(operation_id,seq)
);
CREATE INDEX IF NOT EXISTS idx_assistant_queued ON assistant_operations(state,created_at);

-- Local operator declarations, atomically committed with one manual settlement.
CREATE TABLE IF NOT EXISTS assistant_resolutions (
 resolution_id TEXT PRIMARY KEY, input_digest TEXT NOT NULL,
 user_id TEXT NOT NULL REFERENCES users(id),
 operation_id TEXT NOT NULL REFERENCES assistant_operations(id),
 operator TEXT NOT NULL, operator_authentication TEXT NOT NULL
   CHECK(operator_authentication='local_declaration'),
 reason TEXT NOT NULL, evidence_reference_json TEXT NOT NULL,
 input_summary_json TEXT NOT NULL, before_json TEXT NOT NULL,
 after_json TEXT NOT NULL, result_json TEXT NOT NULL, created_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_assistant_active_budget ON assistant_turn_budgets(user_id) WHERE state='active';

-- GLM reasoning stays server-side, bound to the original settled operation.
CREATE TABLE IF NOT EXISTS assistant_glm_messages (
 message_id TEXT PRIMARY KEY,
 operation_id TEXT NOT NULL UNIQUE REFERENCES assistant_operations(id),
 model TEXT NOT NULL,
 message_digest TEXT NOT NULL,
 reasoning TEXT NOT NULL
);
