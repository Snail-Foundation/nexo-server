-- Order projections for the waiter and kitchen web app.
-- This file is immutable once applied. Changes go in 0004+.
--
-- These tables are a deterministic fold of the operation log. They can be
-- rebuilt from the log plus a snapshot.

CREATE TABLE restaurant_order (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    branch_id TEXT NOT NULL REFERENCES branch(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL REFERENCES device(id),
    order_type TEXT NOT NULL CHECK (order_type IN ('dine_in', 'takeout')),
    table_id TEXT REFERENCES dining_table(id) ON DELETE SET NULL,
    customer_name TEXT,
    display_code TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK (
        status IN ('open', 'active', 'served', 'billed', 'cancelled')
    ),
    notes TEXT,
    opened_by TEXT REFERENCES app_user(id) ON DELETE SET NULL,
    opened_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER,
    cancel_reason TEXT,
    CHECK (
        (order_type = 'dine_in' AND table_id IS NOT NULL)
        OR (order_type = 'takeout' AND table_id IS NULL)
    )
);

CREATE INDEX restaurant_order_branch_idx
    ON restaurant_order(branch_id, status, opened_at_ms);

CREATE INDEX restaurant_order_table_idx
    ON restaurant_order(table_id)
    WHERE status IN ('open', 'active', 'served');

CREATE UNIQUE INDEX restaurant_order_one_open_per_table
    ON restaurant_order(table_id)
    WHERE table_id IS NOT NULL AND status IN ('open', 'active', 'served');

CREATE INDEX restaurant_order_code_idx
    ON restaurant_order(branch_id, display_code);

CREATE TABLE order_line (
    id TEXT PRIMARY KEY NOT NULL,
    order_id TEXT NOT NULL REFERENCES restaurant_order(id) ON DELETE CASCADE,
    menu_item_id TEXT REFERENCES menu_item(id) ON DELETE SET NULL,
    description TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    unit_price_minor INTEGER NOT NULL CHECK (unit_price_minor >= 0),
    line_total_minor INTEGER NOT NULL CHECK (line_total_minor >= 0),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    notes TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (
        status IN ('pending', 'sent', 'ready', 'served', 'voided')
    ),
    round_number INTEGER NOT NULL DEFAULT 1 CHECK (round_number > 0),
    created_at_ms INTEGER NOT NULL,
    sent_at_ms INTEGER,
    ready_at_ms INTEGER,
    served_at_ms INTEGER,
    voided_at_ms INTEGER,
    void_reason TEXT,
    created_by TEXT REFERENCES app_user(id) ON DELETE SET NULL
);

CREATE INDEX order_line_order_idx
    ON order_line(order_id, round_number);

CREATE INDEX order_line_kitchen_idx
    ON order_line(status, sent_at_ms);

CREATE TABLE order_event (
    id TEXT PRIMARY KEY NOT NULL,
    order_id TEXT NOT NULL REFERENCES restaurant_order(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    actor_user_id TEXT REFERENCES app_user(id) ON DELETE SET NULL,
    device_id TEXT NOT NULL REFERENCES device(id),
    details TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX order_event_order_idx
    ON order_event(order_id, created_at_ms);
