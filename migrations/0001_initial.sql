-- Initial Nexo Server migration.
-- This file is immutable once applied. Changes go in 0002+.
--
-- The server is the synchronization hub and the waiter/kitchen web host. It
-- stores the append-only operation log and the auth data needed to accept and
-- serve operations. Business state projections are added in later migrations.

CREATE TABLE business (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    legal_name TEXT,
    tax_id TEXT,
    base_currency TEXT NOT NULL DEFAULT 'USD' CHECK (length(base_currency) = 3),
    locale TEXT NOT NULL DEFAULT 'es' CHECK (locale IN ('es', 'en')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE branch (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    address TEXT,
    is_default INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX branch_one_default_per_business
    ON branch(business_id)
    WHERE is_default = 1;

CREATE TABLE device (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    branch_id TEXT REFERENCES branch(id) ON DELETE SET NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'desktop' CHECK (
        kind IN ('desktop', 'server', 'web')
    ),
    is_master INTEGER NOT NULL DEFAULT 0 CHECK (is_master IN (0, 1)),
    token_hash TEXT UNIQUE,
    registered_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER
);

CREATE UNIQUE INDEX device_one_master_per_business
    ON device(business_id)
    WHERE is_master = 1;

CREATE TABLE app_user (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    role TEXT NOT NULL CHECK (
        role IN ('admin', 'cashier', 'waiter', 'kitchen')
    ),
    pin_hash TEXT,
    is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX app_user_name_per_business
    ON app_user(business_id, display_name);

CREATE TABLE auth_session (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES app_user(id) ON DELETE CASCADE,
    device_id TEXT REFERENCES device(id) ON DELETE SET NULL,
    kind TEXT NOT NULL CHECK (kind IN ('web', 'api')),
    token_hash TEXT NOT NULL UNIQUE,
    csrf_token TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    revoked_at_ms INTEGER,
    last_used_at_ms INTEGER
);

CREATE INDEX auth_session_user_idx
    ON auth_session(user_id, expires_at_ms);

CREATE TABLE server_setting (
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (business_id, key)
);

CREATE TABLE operation_log (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    device_id TEXT NOT NULL,
    branch_id TEXT,
    kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    created_at_ms INTEGER NOT NULL,
    received_at_ms INTEGER NOT NULL
);

CREATE INDEX operation_log_received_idx
    ON operation_log(received_at_ms);

CREATE TABLE device_cursor (
    device_id TEXT PRIMARY KEY NOT NULL,
    last_pulled_seq INTEGER NOT NULL DEFAULT 0 CHECK (last_pulled_seq >= 0),
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE sync_anomaly (
    id TEXT PRIMARY KEY NOT NULL,
    operation_id TEXT,
    device_id TEXT,
    kind TEXT NOT NULL,
    details TEXT NOT NULL CHECK (json_valid(details)),
    created_at_ms INTEGER NOT NULL,
    resolved_at_ms INTEGER,
    resolution_note TEXT
);

CREATE INDEX sync_anomaly_pending_idx
    ON sync_anomaly(resolved_at_ms, created_at_ms);

CREATE TABLE audit_event (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    branch_id TEXT REFERENCES branch(id) ON DELETE SET NULL,
    actor_user_id TEXT REFERENCES app_user(id) ON DELETE SET NULL,
    device_id TEXT NOT NULL,
    action TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    details TEXT CHECK (details IS NULL OR json_valid(details)),
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX audit_event_created_idx
    ON audit_event(created_at_ms);
