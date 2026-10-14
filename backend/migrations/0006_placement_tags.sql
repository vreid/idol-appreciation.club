-- Placement tags: a small normalized label set per placement. Applied by
-- the owner (or an admin) after place, and in bulk by the admin import
-- manifest (optional `tags` per entry). Pure metadata: no tile, seq, or
-- bbox impact, so nothing here participates in cache invalidation.
CREATE TABLE placement_tags
(
    placement_id uuid NOT NULL REFERENCES placements (id) ON DELETE CASCADE,
    tag          text NOT NULL,
    PRIMARY KEY (placement_id, tag)
);

CREATE INDEX placement_tags_tag_idx ON placement_tags (tag);
