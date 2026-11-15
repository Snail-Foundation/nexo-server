-- Master-data projections for the waiter and kitchen web app.
-- This file is immutable once applied. Changes go in 0003+.
--
-- These tables are a deterministic fold of the operation log. They can be
-- rebuilt from the log plus a snapshot.

CREATE TABLE business_setting (
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (business_id, key)
);

CREATE TABLE menu_category (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX menu_category_business_idx
    ON menu_category(business_id, sort_order);

CREATE TABLE menu_item (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    category_id TEXT REFERENCES menu_category(id) ON DELETE SET NULL,
    name TEXT NOT NULL,
    description TEXT,
    price_minor INTEGER NOT NULL CHECK (price_minor >= 0),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    is_available INTEGER NOT NULL DEFAULT 1 CHECK (is_available IN (0, 1)),
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX menu_item_business_idx
    ON menu_item(business_id, sort_order);

CREATE INDEX menu_item_category_idx
    ON menu_item(category_id);

CREATE TABLE dining_table (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    branch_id TEXT REFERENCES branch(id) ON DELETE SET NULL,
    name TEXT NOT NULL,
    zone TEXT,
    is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX dining_table_branch_idx
    ON dining_table(branch_id, zone, name);

CREATE TABLE tax_rate (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    rate_basis_points INTEGER NOT NULL CHECK (
        rate_basis_points BETWEEN 0 AND 10000
    ),
    is_enabled INTEGER NOT NULL DEFAULT 0 CHECK (is_enabled IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX tax_rate_one_per_business
    ON tax_rate(business_id);

CREATE TABLE payment_method (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (
        kind IN ('cash', 'card', 'transfer', 'mobile_payment', 'other')
    ),
    currency TEXT CHECK (currency IS NULL OR length(currency) = 3),
    is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (business_id, name)
);
