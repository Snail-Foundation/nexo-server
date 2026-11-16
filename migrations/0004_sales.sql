-- Sale and cash projections for cashier billing.
-- This file is immutable once applied. Changes go in 0005+.
--
-- These tables are a deterministic fold of the operation log. The desktop
-- remains autonomous: it creates the sale and the cash movements locally and
-- the server only projects them for the web app and for reporting.

CREATE TABLE cash_session (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    branch_id TEXT NOT NULL REFERENCES branch(id),
    device_id TEXT NOT NULL REFERENCES device(id),
    opened_by TEXT REFERENCES app_user(id),
    closed_by TEXT REFERENCES app_user(id),
    opening_amount_minor INTEGER NOT NULL DEFAULT 0 CHECK (
        opening_amount_minor >= 0
    ),
    currency TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    secondary_currency TEXT NOT NULL DEFAULT 'VES' CHECK (
        length(secondary_currency) = 3
    ),
    secondary_opening_amount_minor INTEGER NOT NULL DEFAULT 0 CHECK (
        secondary_opening_amount_minor >= 0
    ),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed')),
    opened_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER,
    expected_amount_minor INTEGER,
    counted_amount_minor INTEGER,
    difference_minor INTEGER,
    secondary_expected_amount_minor INTEGER,
    secondary_counted_amount_minor INTEGER,
    secondary_difference_minor INTEGER,
    notes TEXT
);

CREATE UNIQUE INDEX cash_session_one_open_per_device
    ON cash_session(branch_id, device_id)
    WHERE status = 'open';

CREATE TABLE sale (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    branch_id TEXT NOT NULL REFERENCES branch(id),
    device_id TEXT NOT NULL REFERENCES device(id),
    order_id TEXT NOT NULL UNIQUE REFERENCES restaurant_order(id),
    cash_session_id TEXT REFERENCES cash_session(id) ON DELETE SET NULL,
    number TEXT NOT NULL CHECK (length(number) > 0),
    status TEXT NOT NULL DEFAULT 'completed' CHECK (
        status IN ('completed', 'cancelled')
    ),
    subtotal_minor INTEGER NOT NULL CHECK (subtotal_minor >= 0),
    tax_minor INTEGER NOT NULL DEFAULT 0 CHECK (tax_minor >= 0),
    total_minor INTEGER NOT NULL CHECK (total_minor >= 0),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    tax_name TEXT,
    tax_rate_basis_points INTEGER CHECK (
        tax_rate_basis_points IS NULL
        OR tax_rate_basis_points BETWEEN 0 AND 10000
    ),
    created_at_ms INTEGER NOT NULL,
    created_by TEXT REFERENCES app_user(id),
    cancelled_at_ms INTEGER,
    cancel_reason TEXT,
    UNIQUE (branch_id, number),
    CHECK (subtotal_minor + tax_minor = total_minor)
);

CREATE INDEX sale_created_idx
    ON sale(branch_id, created_at_ms);

CREATE TABLE sale_line (
    id TEXT PRIMARY KEY NOT NULL,
    sale_id TEXT NOT NULL REFERENCES sale(id) ON DELETE CASCADE,
    order_line_id TEXT REFERENCES order_line(id) ON DELETE SET NULL,
    menu_item_id TEXT REFERENCES menu_item(id) ON DELETE SET NULL,
    description TEXT NOT NULL,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    unit_price_minor INTEGER NOT NULL CHECK (unit_price_minor >= 0),
    line_total_minor INTEGER NOT NULL CHECK (line_total_minor >= 0),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX sale_line_sale_idx
    ON sale_line(sale_id);

CREATE TABLE payment (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    sale_id TEXT NOT NULL REFERENCES sale(id) ON DELETE CASCADE,
    method_id TEXT REFERENCES payment_method(id) ON DELETE SET NULL,
    amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    base_amount_minor INTEGER NOT NULL CHECK (base_amount_minor > 0),
    exchange_rate_micros INTEGER CHECK (
        exchange_rate_micros IS NULL OR exchange_rate_micros > 0
    ),
    reference TEXT,
    created_at_ms INTEGER NOT NULL,
    created_by TEXT REFERENCES app_user(id),
    device_id TEXT NOT NULL REFERENCES device(id)
);

CREATE INDEX payment_sale_idx
    ON payment(sale_id);

CREATE TABLE cash_movement (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    cash_session_id TEXT NOT NULL REFERENCES cash_session(id),
    kind TEXT NOT NULL CHECK (
        kind IN ('in', 'out', 'sale', 'refund', 'expense', 'withdrawal', 'deposit')
    ),
    amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    reason TEXT,
    reference_type TEXT,
    reference_id TEXT,
    created_at_ms INTEGER NOT NULL,
    created_by TEXT REFERENCES app_user(id),
    device_id TEXT NOT NULL REFERENCES device(id)
);

CREATE INDEX cash_movement_session_idx
    ON cash_movement(cash_session_id, created_at_ms);
