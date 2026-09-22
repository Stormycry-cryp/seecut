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
    updated_at INTEGER NOT NULL
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
