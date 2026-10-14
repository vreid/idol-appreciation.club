//! Tile pyramid: lazy rendering from chunks, maxSeq staleness, caching.
//!
//! Render admission control (the pile-wedge fix): a tile GET never renders
//! when stored bytes exist — stale bytes are served immediately and a
//! re-render is queued on a single background worker. First-ever renders
//! (no stored tile at all) stay inline so a cold tile always produces
//! bytes, except under render-storm floods, where the queue depth doubles
//! as a congestion signal and cold requests are shed with `202`.

use crate::error::{internal, AppError, AppResult};
use crate::img;
use crate::state::App;
use crate::world::{
    is_valid_tile, tile_bounds, tile_chunk, tile_key, tile_native_size, tile_offset_in_chunk,
    BASE_Z, CHUNK_PX, MAX_Z, TILE_PX,
};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

/// (z, x, y, seq) → encoded webp bytes.
type CacheKey = (i64, i64, i64, i64);

pub struct TileCache {
    /// z ≤ 4 tiles never evict (at most 341 exist).
    low_zoom: StdMutex<HashMap<CacheKey, Bytes>>,
    lru: StdMutex<LruCache<CacheKey, Bytes>>,
}

impl TileCache {
    pub fn new(entries: usize) -> Self {
        Self {
            low_zoom: StdMutex::new(HashMap::new()),
            lru: StdMutex::new(LruCache::new(
                NonZeroUsize::new(entries.max(1)).unwrap_or(NonZeroUsize::MIN),
            )),
        }
    }

    fn get(&self, z: i64, key: CacheKey) -> Option<Bytes> {
        if z <= 4 {
            self.low_zoom
                .lock()
                .expect("low zoom cache")
                .get(&key)
                .cloned()
        } else {
            self.lru.lock().expect("tile lru").get(&key).cloned()
        }
    }

    fn put(&self, z: i64, key: CacheKey, bytes: Bytes) {
        if z <= 4 {
            self.low_zoom
                .lock()
                .expect("low zoom cache")
                .insert(key, bytes);
        } else {
            self.lru.lock().expect("tile lru").put(key, bytes);
        }
    }
}

/// Lease after which a `rendering` row is reclaimable (a worker died
/// mid-render). Generous: a cold z0 pyramid render takes seconds, not
/// minutes, so a reclaim never races a live render.
const RENDER_LEASE_SECS: f64 = 60.0;

/// Bounded, coalescing background re-render queue, persisted in the
/// `tile_render_jobs` table (migration 0005) and drained by a single
/// in-process worker (`spawn_render_worker`). One worker means at most one
/// background render runs at a time, so renders can never monopolize vips
/// / `spawn_blocking` and starve the placement path.
///
/// Firing rule per coord: `min(quiet_window, max_delay)` —
/// - quiet window: no stale GET re-requested the tile for
///   `TILE_RENDER_QUIET_SECS`, so a placement burst in one window folds
///   into ONE render at the latest seq;
/// - max delay: `TILE_RENDER_MAX_DELAY_SECS` since first enqueue — the
///   hard cap that keeps continuously-bumped tiles (a soak writing into
///   one area forever) rendering at a bounded rate instead of starving.
///
/// Overflow policy: enqueue attempts beyond `cap` pending rows are dropped
/// (the next stale GET re-enqueues); the depth doubles as the congestion
/// signal for shedding first-ever renders with `202`.
///
/// The table IS the queue on purpose: a future dedicated render node group
/// (out-of-process worker Deployment, same image, render role) consumes
/// the identical claim query — state + `FOR UPDATE SKIP LOCKED` is the
/// entire coordination protocol, no advisory locks.
pub struct RenderQueue {
    cap: usize,
    pool: sqlx::PgPool,
    dropped: AtomicU64,
}

impl RenderQueue {
    pub fn new(pool: sqlx::PgPool, cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            pool,
            dropped: AtomicU64::new(0),
        }
    }

    /// Record demand for a re-render of (z, x, y) — one roundtrip that
    /// coalesces, bumps, and applies the cap:
    /// - row pending: quiet window resets (`last_bump_at = now()`), cap
    ///   anchor (`enqueued_at`) preserved;
    /// - row done (or absent): re-enqueued fresh;
    /// - row rendering (in-flight): no-op — the render in progress covers
    ///   everything committed before it started rendering, and the next
    ///   stale GET after it completes re-enqueues;
    /// - at cap with no row: dropped (caller served stale bytes anyway).
    pub async fn enqueue(&self, z: i64, x: i64, y: i64) {
        let recorded: Result<(bool, bool), _> = sqlx::query_as(
            "WITH bump AS (
                 UPDATE tile_render_jobs SET last_bump_at = now()
                 WHERE z = $1 AND x = $2 AND y = $3 AND state = 'pending'
                 RETURNING 1
             ), ins AS (
                 INSERT INTO tile_render_jobs (z, x, y)
                 SELECT $1, $2, $3
                 WHERE NOT EXISTS (SELECT 1 FROM bump)
                   AND (SELECT count(*) FROM tile_render_jobs
                        WHERE state = 'pending') < $4
                 ON CONFLICT (z, x, y) DO UPDATE
                 SET state = 'pending', claimed_at = NULL,
                     enqueued_at = now(), last_bump_at = now()
                 WHERE tile_render_jobs.state = 'done'
                 RETURNING 1
             )
             SELECT EXISTS(SELECT 1 FROM bump), EXISTS(SELECT 1 FROM ins)",
        )
        .bind(z as i16)
        .bind(x as i32)
        .bind(y as i32)
        .bind(self.cap as i64)
        .fetch_one(&self.pool)
        .await;
        match recorded {
            Ok((bumped, inserted)) if bumped || inserted => {}
            // Dropped by the cap, raced with an in-flight render, or the
            // query failed — in every case the next stale GET retries.
            _ => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Pending coords — the queue-depth gauge.
    pub async fn depth(&self) -> u64 {
        let (n,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM tile_render_jobs WHERE state = 'pending'")
                .fetch_one(&self.pool)
                .await
                .unwrap_or((0,));
        n.max(0) as u64
    }

    /// Total enqueue attempts not recorded (cap shed, or raced with an
    /// in-flight render) since boot. Retried by the next stale GET.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Congestion signal for first-ever renders: at capacity, shed.
    pub async fn congested(&self) -> bool {
        self.depth().await >= self.cap as u64
    }

    /// Claim the next due job: pending rows past their quiet window or
    /// max-delay cap, plus `rendering` rows past the crash lease (a worker
    /// died mid-render). FIFO by enqueue time; SKIP LOCKED makes concurrent
    /// claimers (today: none — one in-process worker; tomorrow: a render
    /// node group) safe.
    async fn claim_due(
        &self,
        quiet_secs: f64,
        max_delay_secs: f64,
    ) -> AppResult<Option<(i64, i64, i64)>> {
        let row: Option<(i16, i32, i32)> = sqlx::query_as(
            "WITH picked AS (
                 SELECT z, x, y FROM tile_render_jobs
                 WHERE (state = 'pending' AND (
                            last_bump_at <= now() - ($1::float8 * interval '1 second')
                         OR enqueued_at <= now() - ($2::float8 * interval '1 second')))
                     OR (state = 'rendering'
                         AND claimed_at <= now() - ($3::float8 * interval '1 second'))
                 ORDER BY (state = 'rendering') DESC, enqueued_at, z, x, y
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             UPDATE tile_render_jobs j
             SET state = 'rendering', claimed_at = now()
             FROM picked
             WHERE j.z = picked.z AND j.x = picked.x AND j.y = picked.y
             RETURNING j.z, j.x, j.y",
        )
        .bind(quiet_secs)
        .bind(max_delay_secs)
        .bind(RENDER_LEASE_SECS)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(z, x, y)| (i64::from(z), i64::from(x), i64::from(y))))
    }

    /// Seconds until the next pending entry could possibly fire (for the
    /// worker's sleep). None when the queue is empty.
    async fn next_due(&self, quiet_secs: f64, max_delay_secs: f64) -> AppResult<Option<f64>> {
        let row: (Option<f64>,) = sqlx::query_as(
            "SELECT EXTRACT(EPOCH FROM (min_due - now()))::float8 FROM (
                 SELECT min(least(
                     last_bump_at + ($1::float8 * interval '1 second'),
                     enqueued_at + ($2::float8 * interval '1 second'))) AS min_due
                 FROM tile_render_jobs WHERE state = 'pending'
             ) t",
        )
        .bind(quiet_secs)
        .bind(max_delay_secs)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Mark a claimed render complete. A FAILED render deliberately stays
    /// `rendering` until the lease expires: reclaim is the retry, with a
    /// 60 s natural backoff.
    async fn mark_done(&self, z: i64, x: i64, y: i64) {
        let _ = sqlx::query(
            "UPDATE tile_render_jobs SET state = 'done', claimed_at = NULL
             WHERE z = $1 AND x = $2 AND y = $3 AND state = 'rendering'",
        )
        .bind(z as i16)
        .bind(x as i32)
        .bind(y as i32)
        .execute(&self.pool)
        .await;
    }
}

/// Executes one claimed render job. The in-process executor is the default
/// today; a future render node group runs the same job from the same table
/// (its executor just calls the identical resolution path from a separate
/// Deployment).
pub trait RenderExecutor: Send + Sync + 'static {
    fn render<'a>(
        &'a self,
        z: i64,
        x: i64,
        y: i64,
    ) -> futures::future::BoxFuture<'a, AppResult<()>>;
}

/// Default executor: renders in this process through the normal
/// always-fresh resolution path (mem/S3 hit if a concurrent inline render
/// already caught up — the dedup for a coord re-enqueued mid-render).
pub struct InProcessExecutor {
    app: App,
}

impl RenderExecutor for InProcessExecutor {
    fn render<'a>(
        &'a self,
        z: i64,
        x: i64,
        y: i64,
    ) -> futures::future::BoxFuture<'a, AppResult<()>> {
        Box::pin(async move {
            let t0 = Instant::now();
            let pause = RENDER_PAUSE_MS.load(Ordering::Relaxed);
            if pause > 0 {
                tokio::time::sleep(Duration::from_millis(pause)).await;
            }
            let ctx = TileCtx::prefetch(&self.app, z, x, y).await?;
            tile_bytes_inner(&self.app, &ctx, z, x, y, Stale::Never, Admission::Ungated).await?;
            crate::metrics::background_render_done(t0.elapsed());
            Ok(())
        })
    }
}

/// Test seam (precedent: `img::test_png`): makes every background render
/// take at least this long so integration tests get deterministic timing
/// windows. Always 0 in production.
static RENDER_PAUSE_MS: AtomicU64 = AtomicU64::new(0);

#[doc(hidden)]
pub fn set_render_pause_for_tests(ms: u64) {
    RENDER_PAUSE_MS.store(ms, Ordering::Relaxed);
}

static PYRAMID_PAUSE_MS: AtomicU64 = AtomicU64::new(0);

#[doc(hidden)]
pub fn set_pyramid_pause_for_tests(ms: u64) {
    PYRAMID_PAUSE_MS.store(ms, Ordering::Relaxed);
}

/// Spawn the single background render worker. Called once from
/// `build_app`; exits only with the process (same lifecycle as the events
/// listener). A future out-of-process render worker replaces — not joins —
/// this task per node by running the same loop without spawning it.
pub fn spawn_render_worker(app: &App) {
    let app = app.clone();
    tokio::spawn(async move {
        let executor = InProcessExecutor { app: app.clone() };
        render_worker(app, executor).await;
    });
}

async fn render_worker(app: App, executor: impl RenderExecutor) {
    let quiet = app.cfg.tile_render_quiet_secs as f64;
    let max_delay = app.cfg.tile_render_max_delay_secs as f64;
    loop {
        let claimed = match app.render_queue.claim_due(quiet, max_delay).await {
            Ok(Some((z, x, y))) => {
                if let Err(err) = executor.render(z, x, y).await {
                    tracing::warn!(z, x, y, %err, "background tile render failed; lease reclaim will retry");
                } else {
                    app.render_queue.mark_done(z, x, y).await;
                }
                continue;
            }
            Ok(None) => None,
            Err(err) => {
                tracing::error!(%err, "render queue claim failed");
                Some(Duration::from_secs(2))
            }
        };
        // Nothing (reliably) due: sleep until the earliest possible firing
        // instant, capped at 1 s so clock skew, lease reclaims and missed
        // deadlines self-correct quickly.
        let due = claimed.or(app
            .render_queue
            .next_due(quiet, max_delay)
            .await
            .ok()
            .flatten()
            .map(|s| Duration::from_secs_f64(s.max(0.0)))
            .or(Some(Duration::from_secs(1))));
        let sleep = due
            .unwrap_or(Duration::from_secs(1))
            .min(Duration::from_secs(1));
        tokio::time::sleep(sleep.max(Duration::from_millis(20))).await;
    }
}

/// GET /api/tiles/{z}/{x}/{y}.webp
pub async fn get_tile(
    State(app): State<App>,
    axum::extract::Path((z, x, y)): axum::extract::Path<(String, String, String)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    // axum 0.8 has no suffix captures, so the last segment is a wildcard
    // that must be exactly "<n>.webp".
    let parse = |s: &str| {
        if s.contains('/') {
            return None;
        }
        s.strip_suffix(".webp").unwrap_or(s).parse::<i64>().ok()
    };
    let (Some(z), Some(x), Some(y)) = (parse(&z), parse(&x), parse(&y)) else {
        return Err(AppError::NotFound);
    };
    if !is_valid_tile(z, x, y) {
        return Err(AppError::NotFound);
    }
    // One-shot snapshot for this render (issue #120): two short pool
    // holds, then the walk runs connection-free.
    let ctx = TileCtx::prefetch(&app, z, x, y).await?;
    let resolved = tile_bytes(&app, &ctx, z, x, y, Stale::Allow, Admission::Gated).await?;
    let (bytes, seq, stale) = match resolved {
        TileResolved::Bytes { bytes, seq, stale } => (bytes, seq, stale),
        // Render-storm flood: queue at capacity, no stored bytes to serve.
        // Shed instead of rendering inline (bounded CPU); the client's
        // next revalidation renders normally once the backlog drains.
        TileResolved::Shed => {
            return Ok((
                StatusCode::ACCEPTED,
                [
                    (header::RETRY_AFTER, "2"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
            )
                .into_response())
        }
    };
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());
    Ok(tile_response(bytes, seq, stale, inm))
}

/// 304 when If-None-Match carries the served `"maxSeq"` tag, else 200 with
/// the encoded tile. Caching is four cooperating parts: `max-age=1` stops
/// per-tile revalidation storms during placement bursts,
/// `stale-while-revalidate` serves tiles instantly while refreshing them in
/// the background, the SSE-driven client refetch bounds visible staleness
/// to ~1s past each place/remove commit, and the server-side stale-serve
/// (`stale` ⇒ `X-Tile-Stale: true` + the STORED seq as ETag) keeps a GET
/// with stale bytes available entirely off the render path — the ETag
/// still makes every revalidation a cheap 304 while the seq is unchanged.
fn tile_response(bytes: Bytes, seq: i64, stale: bool, if_none_match: Option<&str>) -> Response {
    let etag = format!("\"{seq}\"");
    let matched = if_none_match.is_some_and(|inm| inm.split(',').any(|t| t.trim() == etag));
    if matched {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        let h = resp.headers_mut();
        h.insert(header::ETAG, etag.parse().expect("etag header"));
        if stale {
            h.insert(
                HeaderName::from_static("x-tile-stale"),
                HeaderValue::from_static("true"),
            );
        }
        return resp;
    }
    let mut resp = (StatusCode::OK, bytes).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, "image/webp".parse().expect("mime"));
    h.insert(
        header::CACHE_CONTROL,
        "public, max-age=1, stale-while-revalidate=30"
            .parse()
            .expect("cache-control header"),
    );
    h.insert(header::ETAG, etag.parse().expect("etag header"));
    if stale {
        h.insert(
            HeaderName::from_static("x-tile-stale"),
            HeaderValue::from_static("true"),
        );
    }
    resp
}

/// The constant empty (pure paper) tile, encoded once per process. Tiles
/// with maxSeq 0 are exactly this — which is what keeps cold pyramid
/// renders O(spine) instead of 4^z.
static EMPTY_TILE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

fn empty_tile_bytes() -> AppResult<Vec<u8>> {
    let bytes = EMPTY_TILE.get_or_init(|| img::empty_tile_256().expect("empty tile encode"));
    Ok(bytes.clone())
}

/// Resolve tile bytes through: memory cache → object storage (if fresh or
/// stale-serveable) → render. Boxed to permit recursion (children of
/// z < BASE_Z render through this same fn).
fn tile_bytes<'a>(
    app: &'a App,
    ctx: &'a TileCtx,
    z: i64,
    x: i64,
    y: i64,
    stale: Stale,
    admission: Admission,
) -> futures::future::BoxFuture<'a, AppResult<TileResolved>> {
    Box::pin(tile_bytes_inner(app, ctx, z, x, y, stale, admission))
}

/// Whether the caller tolerates serving stored-but-stale bytes. Entry GETs
/// allow it; every actual render (inline or background, including parent
/// renders resolving children) must be pixel-fresh, so children always use
/// [`Stale::Never`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stale {
    Allow,
    Never,
}

/// Whether this resolution may acquire a pyramid lease. Only entry GETs
/// are gated: a cold z < BASE_Z render is a multi-minute sequential
/// recursion (issue #120), and N concurrent entries must not pin N pool
/// connections for that long. Recursion children resolve under the
/// parent's existing lease; the background worker is serial by
/// construction; prewarm only walks z >= BASE_Z leaves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    Gated,
    Ungated,
}

/// Bound on concurrent full-pyramid (z < BASE_Z) renders. Held for the
/// whole recursion — with one query in flight per render (sequential
/// children), this is also the max pool footprint of all pyramid renders
/// combined. Entry GETs that find the gate full are shed (202 + Retry-
/// After, the existing shed path) instead of queueing invisibly.
pub struct PyramidGate {
    permits: std::sync::Arc<tokio::sync::Semaphore>,
}

impl PyramidGate {
    pub fn new(permits: usize) -> Self {
        Self {
            permits: std::sync::Arc::new(tokio::sync::Semaphore::new(permits)),
        }
    }

    /// Take a lease if one is free; `None` means shed. The gauge is
    /// decremented when the lease drops.
    pub fn try_acquire(&self) -> Option<PyramidLease> {
        match self.permits.clone().try_acquire_owned() {
            Ok(permit) => {
                crate::metrics::pyramid_acquired();
                Some(PyramidLease {
                    _permit: Some(permit),
                })
            }
            Err(_) => {
                crate::metrics::pyramid_shed();
                None
            }
        }
    }
}

/// RAII marker for one in-flight pyramid render.
pub struct PyramidLease {
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Drop for PyramidLease {
    fn drop(&mut self) {
        if self._permit.take().is_some() {
            crate::metrics::pyramid_released();
        }
    }
}

/// One-shot DB snapshot for a render (issue #120): everything the whole
/// recursion needs, fetched in short pool holds and RELEASED before the
/// walk. The walk itself is connection-free — pure Rust, object storage,
/// pixels. This is the structural fix for the pool starvation: a render
/// never holds a connection across its external I/O (S3/vips), so N
/// concurrent renders pin zero idle connections.
///
/// Snapshot semantics are deliberately NOT transactional-perfect: we
/// snapshot rows + seqs, render, and store at the snapshot seq. A
/// placement landing mid-render bumps the covering seq, the next GET
/// sees stored < covering and serves stale + re-enqueues — the system
/// self-heals around the stale snapshot. Do NOT re-introduce a held
/// connection "for correctness".
/// One zoom level's stored-seq rows from the tiles table.
type StoredLevel = HashMap<(i64, i64), i64>;

struct TileCtx {
    /// Per-chunk max live placement seq for every chunk intersecting the
    /// root rect (ONE grouped query over the GiST index). Chunk
    /// granularity is a conservative superset of per-leaf attribution: a
    /// placement touching any part of a chunk marks all its leaves.
    chunk_seqs: HashMap<(i64, i64), i64>,
    /// Stored staleness rows (tiles table), fetched lazily ONE level at a
    /// time: a level's `(x, y) -> seq` map is materialized at most once
    /// per render, on a brief pool hold, the first time a node at that
    /// level needs its stored seq (mem-cache hits skip it entirely).
    stored_levels: StdMutex<HashMap<i64, Arc<StoredLevel>>>,
    /// Memoized effective seq per node = max over its subtree chunks.
    node_seqs: StdMutex<HashMap<(i64, i64, i64), i64>>,
    /// The zoom this context was prefetched for (level-span arithmetic
    /// and the "strictly under the root" check key off it).
    root_z: i64,
    /// The root tile's own staleness row (the lazy level fetches only
    /// cover strictly deeper levels).
    root_stored: Option<i64>,
}

impl TileCtx {
    async fn prefetch(app: &App, z: i64, x: i64, y: i64) -> AppResult<Self> {
        let rect = tile_bounds(z, x, y);
        // Per-chunk max live placement seq: a PLAIN GiST-probe retrieval
        // (Index Scan on placements_bbox_gist, ~0.1 ms for tile-sized
        // boxes on the 51k-row prod pile), with the chunk-span expansion
        // and per-chunk max done in Rust. Never aggregate this in SQL:
        // `max(seq)` lets the planner pick a placements_seq_key BACKWARD
        // scan that filters bbox row-by-row (51750 rows removed, ~150
        // ms-1.3 s per probe — the 2026-10-14 pile-wedge shape), and a
        // CROSS JOIN LATERAL generate_series expansion poisons the row
        // estimate (~149M planned rows, ~1.1 s). Prod-verified plans in
        // PR #134.
        let rows: Vec<(i64, i64, i64, i32, i32)> = sqlx::query_as(
            "SELECT seq, x, y, w, h
             FROM placements
             WHERE removed_at IS NULL
               AND bbox && box(point($1::float8, $2::float8),
                               point($3::float8, $4::float8))",
        )
        .bind(rect.x)
        .bind(rect.y)
        .bind(rect.x + rect.w - 1)
        .bind(rect.y + rect.h - 1)
        .fetch_all(&app.pool)
        .await?;
        let mut chunk_seqs: HashMap<(i64, i64), i64> = HashMap::new();
        for (seq, x, y, w, h) in rows {
            let (w, h) = (i64::from(w), i64::from(h));
            let cx0 = x / CHUNK_PX;
            let cx1 = (x + w - 1) / CHUNK_PX;
            let cy0 = y / CHUNK_PX;
            let cy1 = (y + h - 1) / CHUNK_PX;
            for cx in cx0..=cx1 {
                for cy in cy0..=cy1 {
                    let slot = chunk_seqs.entry((cx, cy)).or_default();
                    if seq > *slot {
                        *slot = seq;
                    }
                }
            }
        }
        Ok(Self {
            chunk_seqs,
            stored_levels: StdMutex::new(HashMap::new()),
            node_seqs: StdMutex::new(HashMap::new()),
            root_z: z,
            root_stored: sqlx::query_as("SELECT seq FROM tiles WHERE z = $1 AND x = $2 AND y = $3")
                .bind(z as i16)
                .bind(x as i32)
                .bind(y as i32)
                .fetch_optional(&app.pool)
                .await?
                .map(|(seq,)| seq),
        })
    }

    /// Effective live seq at a node = max chunk seq over its subtree.
    /// Memoized: every node of the pyramid is computed exactly once.
    fn node_seq(&self, z: i64, x: i64, y: i64) -> i64 {
        {
            let memo = self.node_seqs.lock().expect("node seq memo");
            if let Some(seq) = memo.get(&(z, x, y)) {
                return *seq;
            }
        }
        let seq = if z >= BASE_Z {
            *self.chunk_seqs.get(&tile_chunk(z, x, y)).unwrap_or(&0)
        } else {
            [
                (2 * x, 2 * y),
                (2 * x + 1, 2 * y),
                (2 * x, 2 * y + 1),
                (2 * x + 1, 2 * y + 1),
            ]
            .into_iter()
            .map(|(cx, cy)| self.node_seq(z + 1, cx, cy))
            .max()
            .unwrap_or(0)
        };
        self.node_seqs
            .lock()
            .expect("node seq memo")
            .insert((z, x, y), seq);
        seq
    }

    /// Stored staleness row for a node, materializing its level lazily.
    async fn stored_seq(
        &self,
        app: &App,
        z: i64,
        x: i64,
        y: i64,
        root_z: i64,
    ) -> AppResult<Option<i64>> {
        if z == root_z {
            return Ok(self.root_stored);
        }
        if z < root_z {
            return Ok(None);
        }
        let level = {
            let levels = self.stored_levels.lock().expect("stored levels");
            levels.get(&z).cloned()
        };
        let level = match level {
            Some(l) => l,
            None => {
                let span = 1i64 << (z - root_z);
                let rows: Vec<(i32, i32, i64)> = sqlx::query_as(
                    "SELECT x, y, seq FROM tiles
                     WHERE z = $1 AND x >= $2 AND x < $3 AND y >= $4 AND y < $5",
                )
                .bind(z as i16)
                .bind((x * span) as i32)
                .bind(((x + 1) * span) as i32)
                .bind((y * span) as i32)
                .bind(((y + 1) * span) as i32)
                .fetch_all(&app.pool)
                .await?;
                let map: HashMap<(i64, i64), i64> = rows
                    .into_iter()
                    .map(|(tx, ty, seq)| ((tx as i64, ty as i64), seq))
                    .collect();
                let map = Arc::new(map);
                self.stored_levels
                    .lock()
                    .expect("stored levels")
                    .insert(z, map.clone());
                map
            }
        };
        Ok(level.get(&(x, y)).copied())
    }
}

/// Outcome of a resolution: bytes with the seq they correspond to (the
/// STORED seq when stale-served — the ETag always matches the bytes) and
/// whether they are stale, or a shed signal for flood control.
enum TileResolved {
    Bytes { bytes: Bytes, seq: i64, stale: bool },
    Shed,
}

// Records its resolution path (hit_mem / hit_s3 / render / empty /
// stale_served / shed) into metrics on every success return, including
// recursive child resolutions.
async fn tile_bytes_inner(
    app: &App,
    ctx: &TileCtx,
    z: i64,
    x: i64,
    y: i64,
    stale: Stale,
    admission: Admission,
) -> AppResult<TileResolved> {
    let t0 = std::time::Instant::now();
    // Snapshot lookup — no pool touch. See TileCtx for the semantics.
    let seq = ctx.node_seq(z, x, y);
    // Empty region: constant paper tile, no recursion, no storage writes.
    // Zero-cost placeholder — served even under congestion (it never
    // queues and never renders).
    if seq == 0 {
        let bytes = tokio::task::spawn_blocking(empty_tile_bytes)
            .await
            .map_err(internal)??;
        crate::metrics::tile_done(crate::metrics::tile_outcome("empty"), t0.elapsed());
        return Ok(TileResolved::Bytes {
            bytes: Bytes::from(bytes),
            seq: 0,
            stale: false,
        });
    }
    let key: CacheKey = (z, x, y, seq);
    if let Some(bytes) = app.tile_cache.get(z, key) {
        crate::metrics::tile_done(crate::metrics::tile_outcome("hit_mem"), t0.elapsed());
        return Ok(TileResolved::Bytes {
            bytes,
            seq,
            stale: false,
        });
    }
    // Serialize renders per tile so concurrent misses don't duplicate work.
    // In-process only, by design: cross-instance duplicate renders are
    // benign — every instance computes the same seq for the tile, so both
    // produce byte-identical output and the last PUT wins with identical
    // content (place compositing is the only nondeterministic raster
    // writer, and it takes the chunk advisory locks). No PG advisory locks
    // on this path.
    let lock_key = format!("{z}/{x}/{y}");
    let _guard = app.tile_locks.lock_sorted(&[lock_key]).await;
    // Another request may have rendered this exact seq while we waited.
    if let Some(bytes) = app.tile_cache.get(z, key) {
        crate::metrics::tile_done(crate::metrics::tile_outcome("hit_mem"), t0.elapsed());
        return Ok(TileResolved::Bytes {
            bytes,
            seq,
            stale: false,
        });
    }
    let object_key = tile_key(z, x, y);
    // Lazy per-level stored-seq snapshot; brief pool hold only the first
    // time this level is touched in this render (mem hits skip it).
    let stored = ctx.stored_seq(app, z, x, y, ctx.root_z).await?;
    if stored == Some(seq) {
        if let Some(bytes) = crate::store::get_bytes(&app.store, &object_key).await? {
            let bytes = Bytes::from(bytes);
            app.tile_cache.put(z, key, bytes.clone());
            crate::metrics::tile_done(crate::metrics::tile_outcome("hit_s3"), t0.elapsed());
            return Ok(TileResolved::Bytes {
                bytes,
                seq,
                stale: false,
            });
        }
    }
    // Stored tile at an older seq: serve the stored bytes NOW and queue a
    // re-render on the background worker. This is the core SWR bound: a
    // GET with stale bytes available never waits on (or triggers) pixel
    // work. A row without its object falls through to a plain render.
    if stale == Stale::Allow {
        if let Some(stored_seq) = stored {
            if let Some(bytes) = crate::store::get_bytes(&app.store, &object_key).await? {
                // Dropping here is fine: the next stale GET re-enqueues.
                app.render_queue.enqueue(z, x, y).await;
                crate::metrics::tile_done(
                    crate::metrics::tile_outcome("stale_served"),
                    t0.elapsed(),
                );
                return Ok(TileResolved::Bytes {
                    bytes: Bytes::from(bytes),
                    seq: stored_seq,
                    stale: true,
                });
            }
        } else if app.render_queue.congested().await {
            // First-ever render with the queue at capacity: shed. Only
            // entry GETs get here (children always render fresh once the
            // parent's inline render was admitted).
            crate::metrics::tile_done(crate::metrics::tile_outcome("shed"), t0.elapsed());
            return Ok(TileResolved::Shed);
        }
    }
    // Pyramid gate (issue #120): a cold z < BASE_Z render is a
    // multi-minute sequential recursion. Entry GETs hold one of a couple
    // of leases for the whole recursion — with sequential children that
    // is also the render's entire pool footprint (at most one query in
    // flight). Entry GETs arriving when the gate is full are shed
    // (202 + Retry-After) instead of stacking a fourth minute-long
    // recursion onto the pool.
    let _pyramid_lease = if admission == Admission::Gated && z < BASE_Z {
        match app.pyramid_gate.try_acquire() {
            Some(lease) => Some(lease),
            None => {
                crate::metrics::tile_done(crate::metrics::tile_outcome("shed"), t0.elapsed());
                return Ok(TileResolved::Shed);
            }
        }
    } else {
        None
    };
    let encoded = render_tile(app, ctx, z, x, y).await?;
    // Write-through: object storage + staleness row + memory cache.
    crate::store::put(&app.store, &object_key, encoded.to_vec()).await?;
    sqlx::query(
        "INSERT INTO tiles (z, x, y, seq) VALUES ($1, $2, $3, $4)
         ON CONFLICT (z, x, y) DO UPDATE
         SET seq = EXCLUDED.seq, rendered_at = now()",
    )
    .bind(z as i16)
    .bind(x as i32)
    .bind(y as i32)
    .bind(seq)
    .execute(&app.pool)
    .await?;
    app.tile_cache.put(z, key, encoded.clone());
    crate::metrics::tile_done(crate::metrics::tile_outcome("render"), t0.elapsed());
    Ok(TileResolved::Bytes {
        bytes: encoded,
        seq,
        stale: false,
    })
}

/// Render + encode the tile from its sources (chunk for z ≥ BASE_Z, the
/// four child tiles below). Pixel work is boxed into spawn_blocking.
async fn render_tile(app: &App, ctx: &TileCtx, z: i64, x: i64, y: i64) -> AppResult<Bytes> {
    // Test seam: slow THIS pyramid render (after the snapshot, before any
    // child resolution) so gate/shed windows are deterministic. Parent
    // renders only; always 0 in production.
    if z < BASE_Z {
        let pause = PYRAMID_PAUSE_MS.load(Ordering::Relaxed);
        if pause > 0 {
            tokio::time::sleep(Duration::from_millis(pause)).await;
        }
    }
    let bytes = if z >= BASE_Z {
        let (cx, cy) = tile_chunk(z, x, y);
        let key = crate::world::chunk_key(cx, cy);
        let chunk = crate::store::get_bytes(&app.store, &key).await?;
        let (ox, oy) = tile_offset_in_chunk(z, x, y);
        let native = tile_native_size(z);
        tokio::task::spawn_blocking(move || img::render_leaf_tile(chunk, ox, oy, native))
            .await
            .map_err(internal)?
            .map_err(internal)?
    } else {
        // Resolve the four children SEQUENTIALLY (issue #120): the old
        // try_join_all put the whole subtree's queries in flight at once —
        // a cold z9 render raced the pool with up to 4^9 pending
        // acquisitions and starved every other handler. One child at a
        // time keeps a render's pool footprint at exactly one in-flight
        // query; the raster cache absorbs 3 of the 4 consecutive leaves
        // (same chunk). Children ALWAYS resolve fresh (Stale::Never) so a
        // parent render never bakes stale child bytes under a fresh parent
        // seq. Deadlock-free: the parent holds "z/x/y" while children take
        // "z+1/x'/y'" keys — the wait-for graph only ever points DOWN the
        // pyramid (a child's own recursion goes deeper, never back up), so
        // no cycle is possible regardless of scheduling. Children run
        // under the parent's pyramid lease (Admission::Ungated): they
        // never shed.
        let mut kids: Vec<Vec<u8>> = Vec::with_capacity(4);
        for (dx, dy) in [(0i64, 0i64), (1, 0), (0, 1), (1, 1)] {
            match tile_bytes(
                app,
                ctx,
                z + 1,
                2 * x + dx,
                2 * y + dy,
                Stale::Never,
                Admission::Ungated,
            )
            .await?
            {
                TileResolved::Bytes { bytes, .. } => kids.push(bytes.to_vec()),
                // Children always resolve with Stale::Never, which never
                // sheds.
                TileResolved::Shed => unreachable!("child resolution never sheds"),
            }
        }
        let kids: [Vec<u8>; 4] = kids.try_into().map_err(|_| internal("children count"))?;
        tokio::task::spawn_blocking(move || img::render_parent_tile(&kids))
            .await
            .map_err(internal)?
            .map_err(internal)?
    };
    Ok(Bytes::from(bytes))
}

/// TILE_PX must divide a chunk exactly at BASE_Z for the recursion to line
/// up (compile-time sanity of the world constants).
const _: () = assert!(TILE_PX * (1 << (20 - BASE_Z)) == CHUNK_PX);

/// Post-import prewarm: render the z18..z20 tiles covering `chunks` through
/// the NORMAL serving path (memory → object storage → render + staleness
/// row), so prewarmed tiles are indistinguishable from lazily served ones.
/// Opt-in via the import request's `prewarm` flag and strictly post-commit:
/// tiles are never rendered while a batch is still ingesting (ingest only
/// lets the `tiles.seq` staleness index mark them stale implicitly).
pub async fn prewarm_chunks(app: &App, chunks: &[(i64, i64)]) -> AppResult<u64> {
    let mut rendered = 0u64;
    for &(cx, cy) in chunks {
        for z in BASE_Z..=MAX_Z {
            let span = 1i64 << (z - BASE_Z);
            for dy in 0..span {
                for dx in 0..span {
                    // Stale::Never: prewarm is a render, not a serve — it
                    // always resolves fresh (and never sheds), matching the
                    // parent-render path's guarantees.
                    let tile_ctx =
                        TileCtx::prefetch(app, z, cx * span + dx, cy * span + dy).await?;
                    match tile_bytes(
                        app,
                        &tile_ctx,
                        z,
                        cx * span + dx,
                        cy * span + dy,
                        Stale::Never,
                        Admission::Ungated,
                    )
                    .await?
                    {
                        TileResolved::Bytes { .. } => rendered += 1,
                        TileResolved::Shed => unreachable!("Stale::Never never sheds"),
                    }
                }
            }
        }
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::{tile_response, PyramidGate};
    use axum::http::header;
    use axum::http::StatusCode;
    use bytes::Bytes;

    const BYTES: Bytes = Bytes::from_static(b"tile-bytes");

    #[test]
    fn fresh_get_is_200_with_cache_headers() {
        let resp = tile_response(BYTES.clone(), 7, false, None);
        assert_eq!(resp.status(), StatusCode::OK);
        let h = resp.headers();
        assert_eq!(h[header::CONTENT_TYPE], "image/webp");
        assert_eq!(
            h[header::CACHE_CONTROL],
            "public, max-age=1, stale-while-revalidate=30"
        );
        assert_eq!(h[header::ETAG], "\"7\"");
    }

    #[test]
    fn matching_if_none_match_is_304_with_etag() {
        let resp = tile_response(BYTES.clone(), 7, false, Some("\"7\""));
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(resp.headers()[header::ETAG], "\"7\"");
    }

    #[test]
    fn if_none_match_list_matches_after_trim() {
        let resp = tile_response(BYTES.clone(), 7, false, Some("\"3\", \"7\""));
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    }

    #[test]
    fn stale_if_none_match_still_serves_200() {
        let resp = tile_response(BYTES.clone(), 7, false, Some("\"6\""));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::ETAG], "\"7\"");
    }

    #[test]
    fn stale_serve_flags_both_success_kinds() {
        // Server-side SWR: stored seq served with X-Tile-Stale on 200...
        let resp = tile_response(BYTES.clone(), 6, true, None);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::ETAG], "\"6\"");
        assert_eq!(resp.headers()["x-tile-stale"], "true");
        // ...and on the 304 when the client already holds those bytes.
        let resp = tile_response(BYTES.clone(), 6, true, Some("\"6\""));
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(resp.headers()["x-tile-stale"], "true");
    }

    #[test]
    fn pyramid_gate_bounds_concurrency_and_sheds() {
        let gate = PyramidGate::new(2);
        let a = gate.try_acquire().expect("first lease");
        let b = gate.try_acquire().expect("second lease");
        // Third entry finds the gate full: shed signal, not a wait.
        assert!(gate.try_acquire().is_none());
        drop(b);
        let c = gate.try_acquire().expect("lease after release");
        drop((a, c));
    }
}
