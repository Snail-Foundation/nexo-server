-- Kitchen printing: network thermal printers and their print jobs.
-- This file is immutable once applied. Changes go in 0008+.
--
-- Printers are server-local hardware configuration. Print jobs are operational
-- side effects of sending a round; they are not projections of the operation
-- log. A job stores the ticket data needed to render ESC/POS bytes so a failed
-- or interrupted job can be retried without rebuilding the order.

CREATE TABLE printer (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL DEFAULT 9100 CHECK (port BETWEEN 1 AND 65535),
    paper_width_mm INTEGER NOT NULL DEFAULT 80 CHECK (
        paper_width_mm IN (58, 80)
    ),
    is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (business_id, name)
);

CREATE INDEX printer_business_idx ON printer(business_id, name);

ALTER TABLE menu_category
    ADD COLUMN printer_id TEXT REFERENCES printer(id) ON DELETE SET NULL;

CREATE TABLE print_job (
    id TEXT PRIMARY KEY NOT NULL,
    business_id TEXT NOT NULL REFERENCES business(id) ON DELETE CASCADE,
    printer_id TEXT NOT NULL REFERENCES printer(id) ON DELETE CASCADE,
    order_id TEXT REFERENCES restaurant_order(id) ON DELETE CASCADE,
    round_number INTEGER CHECK (round_number IS NULL OR round_number > 0),
    kind TEXT NOT NULL DEFAULT 'kitchen' CHECK (kind IN ('kitchen', 'test')),
    dedupe_key TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (
        status IN ('pending', 'printing', 'printed', 'failed')
    ),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    created_at_ms INTEGER NOT NULL,
    available_at_ms INTEGER NOT NULL DEFAULT 0,
    claimed_at_ms INTEGER,
    printed_at_ms INTEGER,
    CHECK (
        (kind = 'kitchen' AND order_id IS NOT NULL AND round_number IS NOT NULL)
        OR (kind = 'test' AND order_id IS NULL AND round_number IS NULL)
    )
);

CREATE UNIQUE INDEX print_job_dedupe_idx
    ON print_job(dedupe_key, printer_id);

CREATE INDEX print_job_pending_idx
    ON print_job(status, available_at_ms, created_at_ms);

CREATE INDEX print_job_business_idx
    ON print_job(business_id, created_at_ms);
