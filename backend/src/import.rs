//! POST /api/admin/import — the batch ingest engine, and the bulk
//! preupload that feeds it (`POST /api/admin/import/originals`).
//!
//! SLO: ≥ 10 images/second sustained on one 4-vCPU node (100k stretch:
//! ≤ 3 h). The per-placement HTTP path pays ~4 s server-side per image
//! (place_duration p50): a chunk GET+PUT round trip per placement (~100×
//! write amplification when a pile hits the same chunks), plus a
//! per-placement PG tx, `pg_notify`, and tile-seq bump. The batch engine
//! collapses all of those to per-GROUP costs:
//!
//! - Manifest entries are grouped into CHUNK-DISJOINT connected components
//!   (union-find over shared chunk keys). Entries whose AABBs overlap
//!   always share ≥ 1 chunk, so components never contend on rasters and
//!   can run in parallel with zero cross-locking.
//! - Each component executes under its sorted chunk-key locks (the exact
//!   HTTP-path invariant: lock → allocate seqs → paint → insert → commit),
//!   painting every chunk LOAD-ONCE / COMPOSITE-N / PUT-ONCE via
//!   `img::replay_canvas`, and inserting all rows with one multi-row
//!   `INSERT … SELECT unnest(...) … ON CONFLICT (id) DO NOTHING`.
//! - `seq`s are allocated for a whole group in one `generate_series`
//!   round trip under the group's locks — the "one seq bump per chunk per
//!   batch" of the design doc (a literal shared seq value is impossible:
//!   `placements.seq` is UNIQUE and `ORDER BY seq` IS the paint order for
//!   moderation replay). Still exactly one allocation RT per chunk-group,
//!   down from one per placement.
//! - SSE: one coarse `import` event per block (default 500 entries) with
//!   the union bbox — never per placement. The web client ignores unknown
//!   event types (`sse.ts::parseWorldEvent`), and tiles invalidate through
//!   the normal `tiles.seq` staleness path, so no web change is required.
//! - Tiles are never rendered during ingest: `tiles.seq` falls below the
//!   live maxSeq and marks affected tiles stale implicitly — no dirty
//!   writes. The opt-in `prewarm` flag re-renders z18–z20 for touched
//!   chunks through the normal serving path after the last commit.
//!
//! Resume/idempotency: entry identity is
//! `uuidv5(namespace, uuidv5(batch_id, entry payload))` — re-POSTing the
//! same manifest with the same `batch_id` skips already-committed entries
//! (pre-check + `ON CONFLICT (id) DO NOTHING`), so a killed import resumes
//! without double-placing. Raster paints are at-least-once (same exposure
//! window as the HTTP path: paint before commit), and repainting the same
//! overlay set in the same order is pixel-stable.
//!
//! Body shape: a single JSON document (64 MiB entries cap ≈ >1 M entries
//! at ~150 B each; 100k ≈ 14 MiB). The body is bounded, so incremental
//! parsing buys nothing; entries are processed in sequential blocks of
//! `IMPORT_BLOCK_ENTRIES` (default 500) to bound the pixel/row working
//! set. Larger workloads chunk client-side: distinct `batch_id`s (the
//! scripts derive them deterministically) resume independently.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex as StdMutex;
use std::time::Instant;

use axum::extract::{Multipart, State};
use axum::Json;
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{internal, AppError, AppResult};
use crate::img;
use crate::state::App;
use crate::world::{normalize_rot, rotated_aabb, Rect, CHUNK_PX, WORLD_PX};

/// Body cap for both import routes. Bounds the manifest (`~150 B/entry`
/// ⇒ >1 M entries) and the preupload (per-image `MAX_UPLOAD_BYTES` is
/// enforced per field; ~100 unique idol photos ≈ 30 MiB).
pub const BODY_LIMIT: usize = 256 << 20;

/// Per-request derivative cache limits. Entries in a block reference few
/// unique derivatives in pile-shaped workloads; the cache keeps the whole
/// batch's unique set hot so repeated shas cost zero S3 GETs. Bounded:
/// overflow clears the map (a later block just refetches).
const DERIV_CACHE_MAX_BYTES: u64 = 64 << 20;
const DERIV_CACHE_MAX_ENTRIES: usize = 512;
/// Parallel S3 GETs when prefetching a block's missing derivatives.
const DERIV_PREFETCH_CONCURRENCY: usize = 8;

/// Chunk passes are grouped into COMPOSITE_SUPER_BLOCK² chunk squares so
/// decoded overlays stay cache-warm across the neighboring chunks an
/// entry's AABB spans (spans are ≤ 4×4 chunks at the 4096 derivative cap,
/// ≤ 2×2 at the default 1024 import long edge).
const COMPOSITE_SUPER_BLOCK: i64 = 2;
/// Decoded-overlay byte budget per composite_group call (decoded RGBA is
/// ~4 B/px; a dense super-block of large derivatives evicts rather than
/// balloons). Per in-flight group task, so 2 executors ≈ 2× this.
const COMPOSITE_DECODE_BUDGET: u64 = 256 << 20;

/// uuidv5 namespace for import entry ids (domain-separated from all other
/// v5 uses; the bytes spell "idol-import-entr" and are stable forever).
const IMPORT_ENTRY_NS: Uuid = Uuid::from_bytes([
    0x69, 0x64, 0x6f, 0x6c, 0x2d, 0x69, 0x6d, 0x70, 0x6f, 0x72, 0x74, 0x2d, 0x65, 0x6e, 0x74, 0x72,
]);

// ---------------------------------------------------------------- request

#[derive(Debug, Deserialize)]
pub struct ImportEntry {
    /// sha256 of the stripped original (content address, `originals/…`).
    source_sha: String,
    /// sha256 of the webp derivative (content address,
    /// `derivatives/{sha}.webp`). Optional: resolved from the newest
    /// placement of `source_sha` when omitted — multi-size originals must
    /// pass it explicitly (the preupload response always carries it).
    #[serde(default)]
    derivative_sha: Option<String>,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    /// Clockwise degrees (y-down), normalized server-side to [0, 360).
    #[serde(default)]
    rot: f32,
    /// Optional label set, validated/normalized like the tags PUT.
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportReq {
    /// Client-chosen batch id. Re-POSTing the same manifest with the same
    /// id is the resume path (see module docs).
    batch_id: Uuid,
    /// After the last commit, render z18..z20 tiles for touched chunks
    /// through the normal serving path. Never during ingest.
    #[serde(default)]
    prewarm: bool,
    #[serde(default)]
    entries: Vec<ImportEntry>,
}

#[derive(Debug, Serialize)]
pub struct OriginalInfo {
    pub source_sha: String,
    pub derivative_sha: String,
    pub w: i64,
    pub h: i64,
}

#[derive(Debug, Serialize)]
pub struct ImportSummary {
    batch_id: Uuid,
    imported: u64,
    duplicates: u64,
    chunk_writes: u64,
    blocks: usize,
    components: usize,
    /// imported / chunk_writes for this batch — the amplification collapse
    /// in one number (HTTP path ≈ 1; dense piles reach hundreds).
    chunk_write_amplification: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    prewarmed_tiles: Option<u64>,
    duration_ms: u64,
    images_per_second: f64,
}

// ------------------------------------------------------------- preupload

/// POST /api/admin/import/originals — bulk preupload. Repeated `image`
/// multipart fields each go through the EXACT /api/place pixel stage
/// (`place::process_original`), at the source dimensions capped to
/// `MAX_SIZE`. Both objects land content-addressed; the response mapping
/// feeds the manifest (`source_sha`, `derivative_sha`, `w`, `h`).
pub async fn import_originals(
    State(app): State<App>,
    Authed(admin_id): Authed,
    mut mp: Multipart,
) -> AppResult<Json<Vec<OriginalInfo>>> {
    crate::auth::require_admin(&app, admin_id).await?;
    let mut out = Vec::new();
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("malformed multipart"))?
    {
        if field.name() != Some("image") {
            continue;
        }
        let mut buf: Vec<u8> = Vec::new();
        let mut field = field;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| AppError::BadRequest("bad image part"))?
        {
            if buf.len() + chunk.len() > app.cfg.max_upload_bytes {
                return Err(AppError::TooLarge);
            }
            buf.extend_from_slice(&chunk);
        }
        let pipeline = crate::place::process_original(
            buf,
            None,
            app.cfg.min_size,
            app.cfg.max_size,
            app.cfg.import_derivative_long_edge,
            app.cfg.max_decode_pixels,
        )
        .await?;
        let original_key = format!(
            "originals/{}.{}",
            pipeline.original_sha,
            pipeline.format.ext()
        );
        let image_key = format!("derivatives/{}.webp", pipeline.derivative_sha);
        crate::store::put_if_absent(&app.store, &image_key, pipeline.derivative).await?;
        crate::store::put_if_absent(&app.store, &original_key, pipeline.stripped).await?;
        out.push(OriginalInfo {
            source_sha: pipeline.original_sha,
            derivative_sha: pipeline.derivative_sha,
            w: pipeline.w,
            h: pipeline.h,
        });
    }
    if out.is_empty() {
        return Err(AppError::BadRequest("no image fields"));
    }
    Ok(Json(out))
}

// ---------------------------------------------------------------- planning

/// Validated, geometry-resolved entry. `derivative_sha` is filled by the
/// resolution step before ids are finalized.
struct PrePlan {
    source_sha: String,
    derivative_sha: Option<String>,
    w: i64,
    h: i64,
    rot: f32,
    x: i64,
    y: i64,
    aabb: Rect,
    tags: Vec<String>,
}

struct Plan {
    id: Uuid,
    source_sha: String,
    derivative_sha: String,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    rot: f32,
    aabb: Rect,
    tags: Vec<String>,
}

fn normalize_sha(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

fn plan_entry(e: ImportEntry, min_size: i64, max_size: i64) -> Result<PrePlan, &'static str> {
    let source_sha = normalize_sha(&e.source_sha).ok_or("source_sha must be 64 hex characters")?;
    let derivative_sha = e.derivative_sha.as_deref().and_then(normalize_sha);
    if e.w <= 0 || e.h <= 0 {
        return Err("w and h must be positive");
    }
    let long = e.w.max(e.h);
    if !(min_size..=max_size).contains(&long) {
        return Err("derivative long edge outside configured size bounds");
    }
    if !e.rot.is_finite() {
        return Err("rot must be finite");
    }
    let rot = normalize_rot(e.rot);
    let tags = crate::api::normalize_tags(e.tags)?;
    // Identical semantics to the HTTP path: bounds checked on the rotated
    // AABB, bbox column = the AABB, x/y/w/h = the unrotated rect.
    let aabb = rotated_aabb(e.x, e.y, e.w, e.h, rot);
    if aabb.x < 0 || aabb.y < 0 || aabb.x + aabb.w > WORLD_PX || aabb.y + aabb.h > WORLD_PX {
        return Err("entry out of world bounds");
    }
    Ok(PrePlan {
        source_sha,
        derivative_sha,
        w: e.w,
        h: e.h,
        rot,
        x: e.x,
        y: e.y,
        aabb,
        tags,
    })
}

/// Deterministic entry identity: v5(namespace, v5(batch_id, payload)).
/// Same manifest + same batch_id ⇒ same ids on every run ⇒ resume is a
/// pure skip, and exact duplicate entries (same sha/coords/rot) collapse.
fn entry_id(batch_id: Uuid, p: &PrePlan, derivative_sha: &str) -> Uuid {
    let ns = Uuid::new_v5(&IMPORT_ENTRY_NS, batch_id.as_bytes());
    Uuid::new_v5(
        &ns,
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            p.source_sha, derivative_sha, p.x, p.y, p.w, p.h, p.rot
        )
        .as_bytes(),
    )
}

/// Fill missing `derivative_sha`s from the newest placement of each
/// `source_sha` — one round trip for the whole batch. Unresolvable ⇒ 400
/// (the client must preupload first).
async fn resolve_derivatives(app: &App, plans: &mut [PrePlan]) -> AppResult<()> {
    let unresolved: BTreeSet<String> = plans
        .iter()
        .filter(|p| p.derivative_sha.is_none())
        .map(|p| p.source_sha.clone())
        .collect();
    if unresolved.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = unresolved.into_iter().collect();
    let rows: Vec<(String, String)> = crate::dbq::timed(
        "import",
        sqlx::query_as(
            "SELECT DISTINCT ON (original_sha) original_sha, derivative_sha
         FROM placements
         WHERE removed_at IS NULL AND original_sha = ANY($1::char(64)[])
         ORDER BY original_sha, seq DESC",
        )
        .bind(&list)
        .fetch_all(&app.pool),
    )
    .await?;
    let found: HashMap<String, String> = rows.into_iter().collect();
    for p in plans.iter_mut() {
        if p.derivative_sha.is_none() {
            p.derivative_sha = found.get(&p.source_sha).cloned();
        }
    }
    if let Some(p) = plans.iter().find(|p| p.derivative_sha.is_none()) {
        tracing::warn!(source_sha = %p.source_sha, "import source_sha has no known derivative");
        return Err(AppError::BadRequest(
            "source_sha has no known derivative (preupload originals first)",
        ));
    }
    Ok(())
}

// ------------------------------------------------------- derivative cache

#[derive(Default)]
struct DerivCache {
    inner: StdMutex<DerivMap>,
}

#[derive(Default)]
struct DerivMap {
    map: HashMap<String, Bytes>,
    total: u64,
}

impl DerivCache {
    fn get(&self, sha: &str) -> Option<Bytes> {
        self.inner
            .lock()
            .expect("deriv cache")
            .map
            .get(sha)
            .cloned()
    }

    fn put(&self, sha: String, bytes: Bytes) {
        let mut m = self.inner.lock().expect("deriv cache");
        if m.map.contains_key(&sha) {
            return;
        }
        if m.total + bytes.len() as u64 > DERIV_CACHE_MAX_BYTES
            || m.map.len() >= DERIV_CACHE_MAX_ENTRIES
        {
            m.map.clear();
            m.total = 0;
        }
        m.total += bytes.len() as u64;
        m.map.insert(sha, bytes);
    }
}

async fn prefetch_derivatives(
    app: &App,
    cache: &DerivCache,
    shas: BTreeSet<String>,
) -> AppResult<()> {
    let missing: Vec<String> = shas
        .into_iter()
        .filter(|s| cache.get(s).is_none())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    tracing::debug!(count = missing.len(), "import: fetching derivatives");
    let app = app.clone();
    futures::stream::iter(missing.into_iter().map(|sha| {
        let app = app.clone();
        async move {
            let key = format!("derivatives/{sha}.webp");
            match crate::store::get_bytes(&app.store, &key).await? {
                Some(bytes) => {
                    cache.put(sha, Bytes::from(bytes));
                    Ok(())
                }
                None => {
                    tracing::warn!(sha = %sha, "import: derivative object missing");
                    Err(AppError::BadRequest(
                        "manifest references missing derivatives (preupload originals first)",
                    ))
                }
            }
        }
    }))
    .buffer_unordered(DERIV_PREFETCH_CONCURRENCY)
    .try_collect::<Vec<_>>()
    .await?;
    Ok(())
}

/// The manifest's `w`/`h` must equal the derivative's real dimensions:
/// chunk paints and moderation replays both composite at the bitmap's
/// natural size, so a mismatch would break replay consistency. Verified
/// once per unique sha via a header-only probe.
fn verify_dims(plans: &[Plan], cache: &DerivCache) -> AppResult<()> {
    let mut dims: HashMap<&str, (i64, i64)> = HashMap::new();
    for p in plans {
        let d = match dims.get(p.derivative_sha.as_str()) {
            Some(d) => *d,
            None => {
                let bytes = cache
                    .get(&p.derivative_sha)
                    .ok_or_else(|| internal("derivative evicted mid-batch"))?;
                let d = img::dimensions(&bytes)?;
                dims.insert(&p.derivative_sha, d);
                d
            }
        };
        if d != (p.w, p.h) {
            tracing::warn!(
                sha = %p.derivative_sha,
                manifest_w = p.w,
                manifest_h = p.h,
                actual_w = d.0,
                actual_h = d.1,
                "import: manifest dims mismatch derivative bitmap"
            );
            return Err(AppError::BadRequest(
                "manifest w/h does not match the derivative bitmap",
            ));
        }
    }
    Ok(())
}

// -------------------------------------------------------------- grouping

/// Connected components over entries sharing a chunk (union-find).
/// Deterministic: components ordered by first entry, members ascending.
/// Entries whose AABBs overlap always share ≥ 1 chunk, so components are
/// pixel-disjoint — they can run in parallel with disjoint chunk-key lock
/// sets, and within a component the paint order on every chunk is exactly
/// seq order.
fn components(entries: &[Plan]) -> Vec<Vec<usize>> {
    let n = entries.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut by_chunk: HashMap<(i64, i64), usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        for key in e.aabb.chunk_keys() {
            match by_chunk.get(&key) {
                Some(&j) => {
                    let ri = find(&mut parent, i);
                    let rj = find(&mut parent, j);
                    if ri != rj {
                        parent[ri] = rj;
                    }
                }
                None => {
                    by_chunk.insert(key, i);
                }
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        groups.entry(find(&mut parent, i)).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().collect();
    for g in &mut out {
        g.sort_unstable();
    }
    out.sort_by_key(|g| g[0]);
    out
}

fn group_chunk_keys(entries: &[Plan], group: &[usize]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for &i in group {
        for (cx, cy) in entries[i].aabb.chunk_keys() {
            set.insert(crate::world::chunk_key(cx, cy));
        }
    }
    set.into_iter().collect()
}

fn union_bbox(acc: Option<Rect>, r: Rect) -> Option<Rect> {
    Some(match acc {
        None => r,
        Some(a) => {
            let x1 = (a.x + a.w).max(r.x + r.w);
            let y1 = (a.y + a.h).max(r.y + r.h);
            let x0 = a.x.min(r.x);
            let y0 = a.y.min(r.y);
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        }
    })
}

// -------------------------------------------------------------- execution

#[derive(Debug, Default)]
struct GroupOutcome {
    imported: u64,
    chunk_writes: u64,
    max_seq: i64,
}

#[derive(Debug, Default)]
struct BlockStats {
    imported: u64,
    duplicates: u64,
    chunk_writes: u64,
    components: usize,
    max_seq: i64,
    bbox: Option<Rect>,
}

const INSERT_SQL: &str = "INSERT INTO placements
    (id, account_id, image_key, x, y, w, h, seq, rot, bbox, original_sha, derivative_sha)
SELECT id, account_id, image_key, x, y, w, h, seq, rot,
       box(point(bx0, by0), point(bx1, by1)),
       original_sha, derivative_sha
FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::bigint[], $5::bigint[],
            $6::int[], $7::int[], $8::bigint[], $9::real[],
            $10::float8[], $11::float8[], $12::float8[], $13::float8[],
            $14::char(64)[], $15::char(64)[])
    AS t(id, account_id, image_key, x, y, w, h, seq, rot, bx0, by0, bx1, by1,
         original_sha, derivative_sha)
ON CONFLICT (id) DO NOTHING";

/// Execute one chunk-disjoint group: chunk locks (sorted) → tx → one seq
/// allocation round trip → per-chunk load-once/composite-N/put-once →
/// one multi-row insert → commit. Locks release after commit, exactly
/// like the per-placement pipeline.
async fn run_group(
    app: &App,
    admin_id: Uuid,
    plans: &[Plan],
    group: Vec<usize>,
    cache: &DerivCache,
) -> AppResult<GroupOutcome> {
    let group = &group;
    let lock_keys = group_chunk_keys(plans, group);
    let _guards = app.chunk_locks.lock_sorted(&lock_keys).await;
    let mut tx = app.pool.begin().await?;

    // One seq round trip per group (the design's "one seq bump per chunk
    // per batch"): contiguous, ascending in entry order, allocated under
    // the group's chunk locks — the HTTP-path ordering invariant.
    let n = i64::try_from(group.len()).map_err(|_| internal("group overflow"))?;
    let seqs: Vec<i64> = crate::dbq::timed(
        "import",
        sqlx::query_scalar("SELECT nextval('placement_seq') FROM generate_series(1, $1)")
            .bind(n)
            .fetch_all(&mut *tx),
    )
    .await?;

    // Per-chunk paint, super-blocked (2×2 chunks) so a decoded overlay
    // stays warm across the neighboring chunks its AABB spans: one
    // spawn_blocking per super-block decodes/rotates each unique overlay
    // at most once within the block (bounded by COMPOSITE_DECODE_BUDGET)
    // and encodes each chunk exactly once. Members arrive in seq order, so
    // the compositing order on every chunk is exactly the committed seq
    // order. Pixel output is identical to per-chunk replay_canvas.
    let mut overlay_idx: HashMap<(&str, u32), usize> = HashMap::new();
    let mut overlays: Vec<(Bytes, f32)> = Vec::new();
    let mut chunk_members: BTreeMap<(i64, i64), Vec<img::OverlayPlacement>> = BTreeMap::new();
    for &i in group {
        let p = &plans[i];
        let key = (p.derivative_sha.as_str(), p.rot.to_bits());
        let idx = match overlay_idx.get(&key) {
            Some(&idx) => idx,
            None => {
                let bytes = cache
                    .get(&p.derivative_sha)
                    .ok_or_else(|| internal("derivative evicted mid-batch"))?;
                overlays.push((bytes, p.rot));
                overlay_idx.insert(key, overlays.len() - 1);
                overlays.len() - 1
            }
        };
        for (cx, cy) in p.aabb.chunk_keys() {
            chunk_members.entry((cx, cy)).or_default().push((
                idx,
                p.aabb.x - cx * CHUNK_PX,
                p.aabb.y - cy * CHUNK_PX,
            ));
        }
    }
    let mut super_blocks: BTreeMap<(i64, i64), Vec<(i64, i64)>> = BTreeMap::new();
    for &(cx, cy) in chunk_members.keys() {
        super_blocks
            .entry((
                cx.div_euclid(COMPOSITE_SUPER_BLOCK),
                cy.div_euclid(COMPOSITE_SUPER_BLOCK),
            ))
            .or_default()
            .push((cx, cy));
    }
    let mut out = GroupOutcome::default();
    for blocks in super_blocks.into_values() {
        let mut chunk_list: Vec<img::ChunkPaint> = Vec::with_capacity(blocks.len());
        for (cx, cy) in blocks {
            let canvas =
                crate::store::get_bytes(&app.store, &crate::world::chunk_key(cx, cy)).await?;
            let members = chunk_members.remove(&(cx, cy)).expect("chunk has members");
            chunk_list.push((cx, cy, canvas, members));
        }
        let overlays_sb = overlays.clone();
        let (chunk_list, encoded_list) = {
            let (chunk_list, res) = crate::telemetry::blocking(move || {
                let res = img::composite_group(&overlays_sb, &chunk_list, COMPOSITE_DECODE_BUDGET);
                (chunk_list, res)
            })
            .await
            .map_err(internal)?;
            (chunk_list, res.map_err(internal)?)
        };
        for ((cx, cy, ..), encoded) in chunk_list.iter().zip(encoded_list) {
            crate::store::put(&app.store, &crate::world::chunk_key(*cx, *cy), encoded).await?;
            out.chunk_writes += 1;
            crate::metrics::chunk_composited();
        }
    }

    // Rows: one multi-row insert; conflicts (concurrent duplicate import)
    // are skipped, and rows_affected is the authoritative imported count.
    let len = group.len();
    let mut ids: Vec<Uuid> = Vec::with_capacity(len);
    let mut accounts: Vec<Uuid> = Vec::with_capacity(len);
    let mut keys: Vec<String> = Vec::with_capacity(len);
    let mut xs: Vec<i64> = Vec::with_capacity(len);
    let mut ys: Vec<i64> = Vec::with_capacity(len);
    let mut ws: Vec<i32> = Vec::with_capacity(len);
    let mut hs: Vec<i32> = Vec::with_capacity(len);
    let mut row_seqs: Vec<i64> = Vec::with_capacity(len);
    let mut rots: Vec<f32> = Vec::with_capacity(len);
    let mut bx0: Vec<f64> = Vec::with_capacity(len);
    let mut by0: Vec<f64> = Vec::with_capacity(len);
    let mut bx1: Vec<f64> = Vec::with_capacity(len);
    let mut by1: Vec<f64> = Vec::with_capacity(len);
    let mut oshas: Vec<String> = Vec::with_capacity(len);
    let mut dshas: Vec<String> = Vec::with_capacity(len);
    for (k, &i) in group.iter().enumerate() {
        let p = &plans[i];
        ids.push(p.id);
        accounts.push(admin_id);
        keys.push(format!("derivatives/{}.webp", p.derivative_sha));
        xs.push(p.x);
        ys.push(p.y);
        ws.push(i32::try_from(p.w).map_err(|_| internal("width overflow"))?);
        hs.push(i32::try_from(p.h).map_err(|_| internal("height overflow"))?);
        row_seqs.push(seqs[k]);
        rots.push(p.rot);
        bx0.push(p.aabb.x as f64);
        by0.push(p.aabb.y as f64);
        bx1.push((p.aabb.x + p.aabb.w - 1) as f64);
        by1.push((p.aabb.y + p.aabb.h - 1) as f64);
        oshas.push(p.source_sha.clone());
        dshas.push(p.derivative_sha.clone());
    }
    let inserted = crate::dbq::timed(
        "import",
        sqlx::query(INSERT_SQL)
            .bind(&ids)
            .bind(&accounts)
            .bind(&keys)
            .bind(&xs)
            .bind(&ys)
            .bind(&ws)
            .bind(&hs)
            .bind(&row_seqs)
            .bind(&rots)
            .bind(&bx0)
            .bind(&by0)
            .bind(&bx1)
            .bind(&by1)
            .bind(&oshas)
            .bind(&dshas)
            .execute(&mut *tx),
    )
    .await?
    .rows_affected();
    // Tags ride the same transaction: a resumed chunk re-inserts its
    // placements with ON CONFLICT DO NOTHING, so re-applying tags is a
    // no-op there and an insert for freshly committed rows.
    for (k, &i) in group.iter().enumerate() {
        for tag in &plans[i].tags {
            crate::dbq::timed(
                "import",
                sqlx::query(
                    "INSERT INTO placement_tags (placement_id, tag)
                 VALUES ($1, $2) ON CONFLICT DO NOTHING",
                )
                .bind(ids[k])
                .bind(tag)
                .execute(&mut *tx),
            )
            .await?;
        }
    }
    // Chunk-version rollup advances with the batch (same tx): the tile
    // read path reads chunk_seqs, never placement rows. Per GROUP, not
    // per tag — tagless entries must bump too.
    let bumps: Vec<(i64, i64, i64, i32, i32)> = group
        .iter()
        .enumerate()
        .map(|(k, &i)| {
            let p = &plans[i];
            (
                seqs[k],
                p.aabb.x,
                p.aabb.y,
                p.aabb.w as i32,
                p.aabb.h as i32,
            )
        })
        .collect();
    crate::chunks::bump_chunk_seqs(&mut tx, &bumps).await?;
    tx.commit().await?;

    out.imported = inserted;
    out.max_seq = seqs.last().copied().unwrap_or(0);
    Ok(out)
}

/// Process one block sequentially end-to-end: dedupe → prefetch → dims
/// check → group → run groups (bounded parallelism) → one coarse SSE
/// event. Blocks run strictly sequentially, so chunk overlap across
/// blocks never races.
async fn process_block(
    app: &App,
    admin_id: Uuid,
    batch_id: Uuid,
    block: Vec<Plan>,
    cache: &DerivCache,
) -> AppResult<BlockStats> {
    let mut stats = BlockStats::default();
    if block.is_empty() {
        return Ok(stats);
    }

    // Identity dedupe: exact in-block duplicates, then entries already
    // committed by a previous run/blocks of this batch id.
    let block_len = block.len();
    let mut seen: HashSet<Uuid> = HashSet::with_capacity(block.len());
    let mut fresh: Vec<Plan> = Vec::with_capacity(block.len());
    for p in block {
        if seen.insert(p.id) {
            fresh.push(p);
        }
    }
    if fresh.is_empty() {
        stats.duplicates = block_len as u64;
        return Ok(stats);
    }
    let ids: Vec<Uuid> = fresh.iter().map(|p| p.id).collect();
    let existing: HashSet<Uuid> = crate::dbq::timed(
        "import",
        sqlx::query_as::<_, (Uuid,)>("SELECT id FROM placements WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(&app.pool),
    )
    .await?
    .into_iter()
    .map(|(id,)| id)
    .collect();
    let fresh: Vec<Plan> = fresh
        .into_iter()
        .filter(|p| !existing.contains(&p.id))
        .collect();
    if fresh.is_empty() {
        stats.duplicates = ids.len() as u64;
        return Ok(stats);
    }
    stats.duplicates = (ids.len() - fresh.len()) as u64;

    // Prefetch unique derivatives for the NEW entries only, then verify
    // declared dims against the actual bitmaps.
    let shas: BTreeSet<String> = fresh.iter().map(|p| p.derivative_sha.clone()).collect();
    prefetch_derivatives(app, cache, shas).await?;
    verify_dims(&fresh, cache)?;

    // The block's event bbox: the union of the NEW entries' AABBs.
    stats.bbox = fresh.iter().fold(None, |acc, p| union_bbox(acc, p.aabb));

    let groups = components(&fresh);
    let concurrency = app.cfg.import_group_concurrency.max(1);
    let outcomes: Vec<GroupOutcome> = futures::stream::iter(
        groups
            .into_iter()
            .map(|g| run_group(app, admin_id, &fresh, g, cache)),
    )
    .buffer_unordered(concurrency)
    .try_collect::<Vec<_>>()
    .await?;
    for o in outcomes {
        stats.imported += o.imported;
        stats.chunk_writes += o.chunk_writes;
        stats.components += 1;
        stats.max_seq = stats.max_seq.max(o.max_seq);
    }

    // One coarse invalidate event per block (web ignores unknown event
    // types; tiles invalidate through tiles.seq staleness regardless).
    if stats.imported > 0 {
        let bbox = stats.bbox.expect("imported entries carry bboxes");
        let event = json!({
            "type": "import",
            "batch_id": batch_id,
            "seq": stats.max_seq,
            "count": stats.imported,
            "bbox": [bbox.x, bbox.y, bbox.w, bbox.h],
        })
        .to_string();
        let _: ((),) = crate::dbq::timed(
            "import",
            sqlx::query_as("SELECT pg_notify('events', $1)")
                .bind(&event)
                .fetch_one(&app.pool),
        )
        .await?;
    }
    Ok(stats)
}

// ---------------------------------------------------------------- handler

/// POST /api/admin/import.
pub async fn import(
    State(app): State<App>,
    Authed(admin_id): Authed,
    Json(req): Json<ImportReq>,
) -> AppResult<Json<ImportSummary>> {
    crate::auth::require_admin(&app, admin_id).await?;
    let t0 = Instant::now();

    let mut plans: Vec<PrePlan> = Vec::with_capacity(req.entries.len());
    for (i, e) in req.entries.into_iter().enumerate() {
        plans.push(
            plan_entry(e, app.cfg.min_size, app.cfg.max_size).map_err(|reason| {
                tracing::warn!(entry = i, reason, "import entry rejected");
                AppError::BadRequest("entry rejected")
            })?,
        );
    }
    resolve_derivatives(&app, &mut plans).await?;
    let mut plans: Vec<Plan> = plans
        .into_iter()
        .map(|mut p| {
            let derivative_sha = p.derivative_sha.take().expect("resolved above");
            Plan {
                id: entry_id(req.batch_id, &p, &derivative_sha),
                source_sha: p.source_sha,
                derivative_sha,
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h,
                rot: p.rot,
                aabb: p.aabb,
                tags: p.tags,
            }
        })
        .collect();

    let cache = DerivCache::default();
    let block_size = app.cfg.import_block_entries.max(1);
    let mut summary = ImportSummary {
        batch_id: req.batch_id,
        imported: 0,
        duplicates: 0,
        chunk_writes: 0,
        blocks: 0,
        components: 0,
        chunk_write_amplification: 0.0,
        prewarmed_tiles: None,
        duration_ms: 0,
        images_per_second: 0.0,
    };
    let mut touched: BTreeSet<(i64, i64)> = BTreeSet::new();
    for p in &plans {
        touched.extend(p.aabb.chunk_keys());
    }

    while !plans.is_empty() {
        let rest = plans.split_off(block_size.min(plans.len()));
        let block = std::mem::take(&mut plans);
        let st = match process_block(&app, admin_id, req.batch_id, block, &cache).await {
            Ok(st) => st,
            // Blocks 1..n-1 already committed: fire the metrics on the
            // partial batch before surfacing the error, or the counters
            // silently lose every row that did land.
            Err(e) => {
                finish_metrics(&mut summary, t0);
                return Err(e);
            }
        };
        summary.imported += st.imported;
        summary.duplicates += st.duplicates;
        summary.chunk_writes += st.chunk_writes;
        summary.components += st.components;
        summary.blocks += 1;
        plans = rest;
    }

    // Fire BEFORE the optional prewarm: the numbers describe the batch's
    // commits, and a prewarm failure must not eat them.
    finish_metrics(&mut summary, t0);

    if req.prewarm {
        let chunks: Vec<(i64, i64)> = touched.into_iter().collect();
        summary.prewarmed_tiles = Some(crate::tiles::prewarm_chunks(&app, &chunks).await?);
    }

    Ok(Json(summary))
}

/// Stamp wall time into the summary and record the batch in the metrics.
/// Called exactly once per import POST — on success AND on mid-batch
/// failure — so every committed row is counted exactly once.
fn finish_metrics(summary: &mut ImportSummary, t0: Instant) {
    let elapsed = t0.elapsed();
    crate::metrics::import_done(summary.imported, summary.chunk_writes, elapsed);
    summary.duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let secs = elapsed.as_secs_f64();
    summary.images_per_second = if secs > 0.0 {
        summary.imported as f64 / secs
    } else {
        0.0
    };
    summary.chunk_write_amplification = if summary.chunk_writes == 0 {
        0.0
    } else {
        summary.imported as f64 / summary.chunk_writes as f64
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::chunk_key;

    fn plan(x: i64, y: i64, w: i64, h: i64, rot: f32) -> Plan {
        let rot = normalize_rot(rot);
        let aabb = rotated_aabb(x, y, w, h, rot);
        Plan {
            id: Uuid::nil(),
            source_sha: "a".repeat(64),
            derivative_sha: "b".repeat(64),
            x,
            y,
            w,
            h,
            rot,
            aabb,
            tags: Vec::new(),
        }
    }

    #[test]
    fn entry_id_is_deterministic_and_discriminating() {
        let batch = Uuid::now_v7();
        let pp = |x: i64, h: i64, rot: f32| PrePlan {
            source_sha: "a".into(),
            derivative_sha: None,
            w: 3,
            h,
            rot,
            x,
            y: 2,
            aabb: Rect::new(x, 2, 3, h),
            tags: Vec::new(),
        };
        let a = entry_id(batch, &pp(1, 4, 0.0), "b");
        assert_eq!(a, entry_id(batch, &pp(1, 4, 0.0), "b"));
        // Any payload change (incl. normalized rot) changes the id.
        assert_ne!(a, entry_id(batch, &pp(1, 5, 0.0), "b"));
        assert_ne!(a, entry_id(batch, &pp(1, 4, 90.0), "b"));
        // Resume with the same manifest but a fresh batch id re-places.
        assert_ne!(a, entry_id(Uuid::now_v7(), &pp(1, 4, 0.0), "b"));
    }

    #[test]
    fn components_group_shared_chunks_and_stay_disjoint() {
        // 1024 grid: e0 and e1 share chunk (1,1); e2 sits alone in (9,9).
        let entries = vec![
            plan(1500, 1500, 100, 100, 0.0),
            plan(1550, 1550, 100, 100, 0.0),
            plan(9216, 9216, 64, 64, 0.0),
        ];
        let groups = components(&entries);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], vec![0, 1]);
        assert_eq!(groups[1], vec![2]);
        // Lock sets are disjoint across components.
        let k0: BTreeSet<_> = group_chunk_keys(&entries, &groups[0]).into_iter().collect();
        let k1: BTreeSet<_> = group_chunk_keys(&entries, &groups[1]).into_iter().collect();
        assert!(k0.is_disjoint(&k1));
        // Overlapping rotated entries share the AABB-overlap chunk ⇒ one
        // component even though the raw rects are offset.
        let rotated = vec![
            plan(1500, 1500, 100, 100, 45.0),
            plan(1560, 1500, 100, 100, 45.0),
        ];
        assert_eq!(components(&rotated).len(), 1);
    }

    #[test]
    fn components_single_entry_multi_chunk_is_one_component() {
        // An entry straddling a chunk corner touches 4 chunks but is one
        // component with 4 lock keys — 4 chunk writes for 1 image.
        let entries = vec![plan(1024 - 10, 1024 - 10, 20, 20, 0.0)];
        let groups = components(&entries);
        assert_eq!(groups.len(), 1);
        assert_eq!(group_chunk_keys(&entries, &groups[0]).len(), 4);
        assert_eq!(entries[0].aabb.chunk_keys().len(), 4);
    }

    #[test]
    fn plan_entry_validates() {
        let e = |source_sha: &str, w: i64, h: i64, rot: f32, x: i64, y: i64| ImportEntry {
            source_sha: source_sha.into(),
            derivative_sha: None,
            x,
            y,
            w,
            h,
            rot,
            tags: Vec::new(),
        };
        // Bad sha.
        assert!(plan_entry(e("xyz", 16, 16, 0.0, 0, 0), 16, 4096).is_err());
        // Non-positive dims.
        assert!(plan_entry(e(&"a".repeat(64), 0, 16, 0.0, 0, 0), 16, 4096).is_err());
        // Long edge out of bounds.
        assert!(plan_entry(e(&"a".repeat(64), 8, 8, 0.0, 0, 0), 16, 4096).is_err());
        assert!(plan_entry(e(&"a".repeat(64), 4097, 10, 0.0, 0, 0), 16, 4096).is_err());
        // Non-finite rot.
        assert!(plan_entry(e(&"a".repeat(64), 16, 16, f32::NAN, 0, 0), 16, 4096).is_err());
        // Out of world.
        assert!(plan_entry(e(&"a".repeat(64), 16, 16, 0.0, WORLD_PX - 8, 0), 16, 4096).is_err());
        // Happy path normalizes rot.
        let p = plan_entry(e(&"a".repeat(64), 16, 32, -10.0, 5000, 5000), 16, 4096).unwrap();
        assert_eq!(p.rot, normalize_rot(-10.0));
        assert_eq!(p.aabb.chunk_keys(), vec![(4, 4)]);
    }

    #[test]
    fn union_bbox_unions() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 25, 5, 5);
        let u = union_bbox(Some(a), b).unwrap();
        assert_eq!((u.x, u.y, u.w, u.h), (0, 0, 25, 30));
        assert_eq!(union_bbox(None, a), Some(a));
    }

    #[test]
    fn chunk_paint_offsets_are_chunk_local() {
        // The overlay's local offset is aabb corner minus chunk origin —
        // the same contract composite_placement uses per chunk.
        let p = plan(1500, 2600, 100, 100, 0.0);
        let (cx, cy) = (1, 2);
        assert_eq!(
            (p.aabb.x - cx * CHUNK_PX, p.aabb.y - cy * CHUNK_PX),
            (476, 552)
        );
        assert_eq!(chunk_key(cx, cy), "chunks/1/2.webp");
    }
}
