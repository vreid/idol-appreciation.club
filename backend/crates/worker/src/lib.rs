//! `iac-worker`: the pixel crate — every libvips call (`img`), the
//! render_jobs execution loops, the ingest consumer, the admin import
//! engine (`import`) and the shared pixel engine (`pixel`). Per docs
//! scaling-architecture § Code and images, ONLY this crate depends on
//! libvips.

pub mod ctx;
pub mod img;
pub mod import;
pub mod ingest;
pub mod pixel;
pub mod render_jobs;
