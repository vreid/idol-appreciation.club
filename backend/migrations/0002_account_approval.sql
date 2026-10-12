-- Admin-approved registration: new signups start unapproved; the first
-- bootstrap account (is_admin) is always approved so gated deployments
-- never lock out their admin.
--
-- Backfill: every existing account predates the gate and keeps full
-- access (tiny user count; includes the bootstrap admin and e2e bots).

ALTER TABLE accounts ADD COLUMN approved boolean NOT NULL DEFAULT false;

ALTER TABLE accounts ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();

UPDATE accounts SET approved = true;
