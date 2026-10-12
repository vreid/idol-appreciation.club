-- Initial schema: accounts, placements manifest, reports, tile cache index.

CREATE TABLE accounts
(
    id                uuid PRIMARY KEY,
    username          text        NOT NULL UNIQUE,
    password_hash     text        NOT NULL,
    is_admin          boolean     NOT NULL DEFAULT false,
    last_placement_at timestamptz
);

-- Exactly one admin may exist; guards the "first account is admin" race.
CREATE UNIQUE INDEX accounts_single_admin ON accounts (is_admin) WHERE is_admin;

CREATE SEQUENCE placement_seq;

CREATE TABLE placements
(
    id          uuid PRIMARY KEY,
    account_id  uuid        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    image_key   text        NOT NULL,
    x           bigint      NOT NULL,
    y           bigint      NOT NULL,
    w           integer     NOT NULL,
    h           integer     NOT NULL,
    seq         bigint      NOT NULL DEFAULT nextval('placement_seq'),
    created_at  timestamptz NOT NULL DEFAULT now(),
    removed_at  timestamptz,
    original_sha   char(64) NOT NULL,
    derivative_sha char(64) NOT NULL,
    -- Closed integer bbox [x, x+w-1] x [y, y+h-1]; box && box on closed
    -- integer boxes is exactly half-open rect intersection.
    bbox        box GENERATED ALWAYS AS
        (box(point(x::double precision, y::double precision),
             point((x + w)::double precision - 1,
                   (y + h)::double precision - 1))) STORED,
    CONSTRAINT placements_rect_positive CHECK (w > 0 AND h > 0),
    CONSTRAINT placements_in_world
        CHECK (x >= 0 AND y >= 0
           AND x + w <= 268435456 AND y + h <= 268435456)
);

ALTER SEQUENCE placement_seq OWNED BY placements.seq;

CREATE UNIQUE INDEX placements_seq_key ON placements (seq);
CREATE INDEX placements_bbox_gist ON placements USING gist (bbox);
CREATE INDEX placements_account_idx ON placements (account_id);
CREATE INDEX placements_original_sha_idx ON placements (original_sha)
    WHERE removed_at IS NULL;

CREATE TABLE reports
(
    id           uuid PRIMARY KEY,
    placement_id uuid        NOT NULL REFERENCES placements (id) ON DELETE CASCADE,
    account_id   uuid        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX reports_placement_idx ON reports (placement_id);

-- Staleness index for the tile pyramid: `seq` is the max placement seq that
-- was baked into the rendered tile (0 = rendered empty). A tile object is
-- current iff tiles.seq equals the live maxSeq over the tile bbox.
CREATE TABLE tiles
(
    z           smallint   NOT NULL,
    x           integer    NOT NULL,
    y           integer    NOT NULL,
    seq         bigint     NOT NULL DEFAULT 0,
    rendered_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (z, x, y)
);
