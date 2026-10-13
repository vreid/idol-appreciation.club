//! Live object-storage check against the REAL bucket.
//!
//! Gated: set `S3_LIVE_CHECK=1` plus the normal S3_* env (the same vars the
//! server uses) to run. Skipped everywhere else, so local/CI default runs
//! never need cloud credentials:
//!
//! ```sh
//! S3_LIVE_CHECK=1 cargo test --test s3_live -- --ignored --nocapture
//! ```
//!
//! Exercises the production write path (`store::put`, `store::put_if_absent`,
//! `store::get_bytes`), which uses fully buffered known-length payloads —
//! required because UpCloud's S3 rejects streamed/chunked SigV4 PUTs.

use idol_appreciation_club_backend::config::Config;
use idol_appreciation_club_backend::store;

fn config() -> Config {
    Config::from_env()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hits the real S3 bucket; run with S3_LIVE_CHECK=1 and S3_* env"]
async fn s3_live_put_get_roundtrip() {
    if std::env::var("S3_LIVE_CHECK").as_deref() != Ok("1") {
        eprintln!("skipped: set S3_LIVE_CHECK=1 with S3_* env to run");
        return;
    }
    let cfg = config();
    let st = store::from_config(&cfg).expect("store from env");

    let blob: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let key = format!("live-check/{}.bin", uuid::Uuid::now_v7());

    // Plain buffered PUT, then read back and compare.
    store::put(&st, &key, blob.clone()).await.expect("put");
    let got = store::get_bytes(&st, &key)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got, blob, "roundtrip mismatch");
    println!("put+get ok: {} bytes at {}", blob.len(), key);

    // Dedupe path: first write wins, second is a no-op.
    let k2 = format!("live-check-dedupe/{}.bin", uuid::Uuid::now_v7());
    assert!(store::put_if_absent(&st, &k2, b"first".to_vec())
        .await
        .expect("put1"));
    assert!(!store::put_if_absent(&st, &k2, b"second".to_vec())
        .await
        .expect("put2"));
    let got = store::get_bytes(&st, &k2)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got, b"first");
    println!("put_if_absent ok: {}", k2);

    // Cleanup so the live bucket stays clean.
    object_store::ObjectStore::delete(&st, &store::key(&key))
        .await
        .expect("del");
    object_store::ObjectStore::delete(&st, &store::key(&k2))
        .await
        .expect("del2");
    println!("cleanup ok");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hits the real S3 bucket; run with S3_LIVE_CHECK=1 and S3_* env"]
async fn s3_list_derivative_count() {
    if std::env::var("S3_LIVE_CHECK").as_deref() != Ok("1") {
        eprintln!("skipped: set S3_LIVE_CHECK=1 with S3_* env to run");
        return;
    }
    // Dedup receipt: N placements of M unique images must keep ~M
    // derivative objects, because derivatives are content-addressed
    // (derivatives/{sha256}.webp).
    let cfg = config();
    let st = store::from_config(&cfg).expect("store from env");
    use futures::StreamExt;
    use object_store::ObjectStore;
    let mut count = 0u32;
    let mut stream = st.list(Some(&store::key("derivatives/")));
    while let Some(meta) = stream.next().await {
        let meta = meta.expect("list derivatives");
        if meta.location.as_ref().ends_with(".webp") {
            count += 1;
        }
    }
    println!("derivative objects: {count}");
    assert!(count > 0, "expected at least one derivative");
    assert!(count < 500, "dedup broken? {count} derivative objects");
}
