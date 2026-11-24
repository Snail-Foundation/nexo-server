-- Cash tendered and change for web billing.
-- This file is immutable once applied. Changes go in 0007+.
--
-- `amount_minor` keeps the applied amount that covers the sale. These optional
-- columns record what the customer handed over and the change returned, so the
-- receipt can show it without changing the sale total.

ALTER TABLE payment ADD COLUMN tendered_minor INTEGER CHECK (
    tendered_minor IS NULL OR tendered_minor > 0
);

ALTER TABLE payment ADD COLUMN change_minor INTEGER CHECK (
    change_minor IS NULL OR change_minor >= 0
);
