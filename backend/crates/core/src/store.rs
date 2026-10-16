//! Object storage: S3-compatible (UpCloud) in prod, `file://` local
//! filesystem for development only.

use crate::config::Config;
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore, ObjectStoreExt, PutMode, PutPayload};
use std::sync::Arc;
use std::time::Instant;

pub type Store = Arc<dyn ObjectStore>;

pub fn from_config(cfg: &Config) -> anyhow::Result<Store> {
    match &cfg.s3_endpoint {
        Some(ep) if ep.starts_with("file://") => {
            // Dev-only mode: bucket maps to a local directory.
            let base = ep.strip_prefix("file://").unwrap_or(ep);
            let dir = std::path::Path::new(base).join(&cfg.s3_bucket);
            std::fs::create_dir_all(&dir)?;
            Ok(Arc::new(
                object_store::local::LocalFileSystem::new_with_prefix(dir)?,
            ))
        }
        Some(ep) => {
            let mut builder = object_store::aws::AmazonS3Builder::from_env().with_endpoint(ep);
            builder = builder
                .with_bucket_name(&cfg.s3_bucket)
                .with_allow_http(true);
            if let (Some(key), Some(secret)) = (&cfg.s3_access_key_id, &cfg.s3_secret_access_key) {
                builder = builder
                    .with_access_key_id(key)
                    .with_secret_access_key(secret);
            }
            Ok(Arc::new(builder.build()?))
        }
        None => anyhow::bail!("S3_ENDPOINT must be set (s3://... or file://... for dev)"),
    }
}

pub fn key(raw: &str) -> ObjPath {
    ObjPath::from(raw)
}

/// Upload unless the content-addressed key already exists (dedupe).
/// Returns true if the object was newly written.
pub async fn put_if_absent(
    store: &Store,
    raw_key: &str,
    bytes: Vec<u8>,
) -> Result<bool, crate::error::AppError> {
    use object_store::PutOptions;
    match store
        .put_opts(
            &key(raw_key),
            PutPayload::from(bytes),
            PutOptions::from(PutMode::Create),
        )
        .await
    {
        Ok(_) => Ok(true),
        Err(object_store::Error::AlreadyExists { .. }) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub async fn get_bytes(
    store: &Store,
    raw_key: &str,
) -> Result<Option<Vec<u8>>, crate::error::AppError> {
    let t0 = Instant::now();
    let res = match store.get(&key(raw_key)).await {
        Ok(resp) => {
            let bytes = resp.bytes().await.map_err(crate::error::internal)?;
            Ok(Some(bytes.to_vec()))
        }
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    };
    crate::metrics::s3_done(0, res.is_ok(), t0.elapsed());
    res
}

pub async fn put(
    store: &Store,
    raw_key: &str,
    bytes: Vec<u8>,
) -> Result<(), crate::error::AppError> {
    let t0 = Instant::now();
    let res: Result<(), crate::error::AppError> = store
        .put(&key(raw_key), PutPayload::from(bytes))
        .await
        .map(|_| ())
        .map_err(Into::into);
    crate::metrics::s3_done(1, res.is_ok(), t0.elapsed());
    res
}
