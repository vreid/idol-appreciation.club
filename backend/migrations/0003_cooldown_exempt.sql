-- Admin-granted per-account cooldown exemption (e.g. seeding/service
-- accounts): exempt accounts skip both cooldowns entirely. Default off;
-- nothing backfilled.
ALTER TABLE accounts ADD COLUMN cooldown_exempt boolean NOT NULL DEFAULT false;
