//! Pure-SQL chunk_seqs version bumps, shared by the API's queue-only
//! placement/removal paths and the worker's ingest consumer. The RASTER
//! mutation lives in the API crate for now (the dark 201 path); these
//! helpers only move rows.

use crate::error::AppResult;

/// Advance the per-chunk content version for every chunk each (seq, x, y,
/// w, h) placement rect touches. Called in the SAME transaction as the
/// placement write (place insert, import batch insert) and — with a fresh
/// `nextval('placement_seq')` as the version — after a removal replay, so
/// versions only move forward. This table is the tile read path's ONLY
/// placement-derived input (issue #120 doctrine): placement rows are never
/// consulted at render time.
pub async fn bump_chunk_seqs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &[(i64, i64, i64, i32, i32)],
) -> AppResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut seqs = Vec::with_capacity(rows.len());
    let mut xs = Vec::with_capacity(rows.len());
    let mut ys = Vec::with_capacity(rows.len());
    let mut ws = Vec::with_capacity(rows.len());
    let mut hs = Vec::with_capacity(rows.len());
    for (s, x, y, w, h) in rows {
        seqs.push(*s);
        xs.push(*x);
        ys.push(*y);
        ws.push(*w);
        hs.push(*h);
    }
    crate::dbq::timed(
        "chunk_bump",
        sqlx::query(
            "INSERT INTO chunk_seqs (cx, cy, max_seq)
             SELECT cx, cy, max(seq)
             FROM unnest($1::bigint[], $2::bigint[], $3::bigint[], $4::int[], $5::int[])
                  AS t(seq, x, y, w, h)
             CROSS JOIN LATERAL generate_series(floor(x / 1024.0)::bigint,
                                                floor((x + w - 1) / 1024.0)::bigint) AS cx
             CROSS JOIN LATERAL generate_series(floor(y / 1024.0)::bigint,
                                                floor((y + h - 1) / 1024.0)::bigint) AS cy
             GROUP BY cx, cy
             ON CONFLICT (cx, cy) DO UPDATE
             SET max_seq = GREATEST(chunk_seqs.max_seq, EXCLUDED.max_seq)",
        )
        .bind(&seqs)
        .bind(&xs)
        .bind(&ys)
        .bind(&ws)
        .bind(&hs)
        .execute(&mut **tx),
    )
    .await?;
    Ok(())
}
/// Pipeline variant of [`bump_chunk_seqs`] (Phase 3,
/// docs/scaling-architecture.md "Chunk and tile pipeline"): identical
/// version advance, plus the ordering safety net — a placement arriving
/// BELOW the chunk's recorded max_seq committed after a higher seq (the
/// render_jobs sequencer lock makes this impossible from the place path;
/// batch imports don't take it), so the stored raster's paint order can't
/// be trusted and the next chunk job must repaint from blank.
pub async fn bump_chunk_seqs_pipeline(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &[(i64, i64, i64, i32, i32)],
) -> AppResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut seqs = Vec::with_capacity(rows.len());
    let mut xs = Vec::with_capacity(rows.len());
    let mut ys = Vec::with_capacity(rows.len());
    let mut ws = Vec::with_capacity(rows.len());
    let mut hs = Vec::with_capacity(rows.len());
    for (s, x, y, w, h) in rows {
        seqs.push(*s);
        xs.push(*x);
        ys.push(*y);
        ws.push(*w);
        hs.push(*h);
    }
    crate::dbq::timed(
        "chunk_bump",
        sqlx::query(
            "INSERT INTO chunk_seqs (cx, cy, max_seq)
             SELECT cx, cy, max(seq)
             FROM unnest($1::bigint[], $2::bigint[], $3::bigint[], $4::int[], $5::int[])
                  AS t(seq, x, y, w, h)
             CROSS JOIN LATERAL generate_series(floor(x / 1024.0)::bigint,
                                                floor((x + w - 1) / 1024.0)::bigint) AS cx
             CROSS JOIN LATERAL generate_series(floor(y / 1024.0)::bigint,
                                                floor((y + h - 1) / 1024.0)::bigint) AS cy
             GROUP BY cx, cy
             ON CONFLICT (cx, cy) DO UPDATE
             SET max_seq = GREATEST(chunk_seqs.max_seq, EXCLUDED.max_seq),
                 needs_rebuild = chunk_seqs.needs_rebuild
                              OR (EXCLUDED.max_seq < chunk_seqs.max_seq)",
        )
        .bind(&seqs)
        .bind(&xs)
        .bind(&ys)
        .bind(&ws)
        .bind(&hs)
        .execute(&mut **tx),
    )
    .await?;
    Ok(())
}
/// Removal-path bump (Phase 3 pipeline): advance every touched chunk's
/// version to a fresh sequence value and flag it for a from-blank
/// rebuild — the removal itself paints nothing; the flagged chunk job
/// replays the remaining live set in sequence order (issue #159's fix,
/// moved off the request path).
pub async fn bump_chunk_seqs_removal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    version: i64,
    chunk_keys: &[(i64, i64)],
) -> AppResult<()> {
    if chunk_keys.is_empty() {
        return Ok(());
    }
    let mut cxs = Vec::with_capacity(chunk_keys.len());
    let mut cys = Vec::with_capacity(chunk_keys.len());
    for &(cx, cy) in chunk_keys {
        cxs.push(cx);
        cys.push(cy);
    }
    crate::dbq::timed(
        "chunk_bump",
        sqlx::query(
            "INSERT INTO chunk_seqs (cx, cy, max_seq, needs_rebuild)
             SELECT cx, cy, $3, true
             FROM unnest($1::bigint[], $2::bigint[]) AS k(cx, cy)
             ON CONFLICT (cx, cy) DO UPDATE
             SET max_seq = GREATEST(chunk_seqs.max_seq, EXCLUDED.max_seq),
                 needs_rebuild = true",
        )
        .bind(&cxs)
        .bind(&cys)
        .bind(version)
        .execute(&mut **tx),
    )
    .await?;
    Ok(())
}
