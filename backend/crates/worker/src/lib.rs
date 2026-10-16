//! `iac-worker`: the pixel crate — every libvips call (`img`), the
//! render_jobs execution loops, the ingest consumer and the shared
//! pixel engine (`pixel`). Per docs/scaling-architecture.md § Code and
//! images, ONLY this crate depends on libvips. It links `iac-core` for
//! the shared foundation; the API crate links this one for now (the
//! dark 201 path and the Phase-2 tile fallback still run pixel work
//! in-process — transitional, removed at the distroless step).

pub mod ctx;
pub mod img;
pub mod ingest;
pub mod pixel;
pub mod render_jobs;
