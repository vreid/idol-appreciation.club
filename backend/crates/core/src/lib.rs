//! `iac-core`: the workspace's shared crate — world math, config, DB
//! query helpers, object store, events listener, metrics and telemetry.
//! Both this crate and the API link it. Per
//! docs/scaling-architecture.md § Code and images, the split keeps the
//! shared, pixel-free foundation in one place so the worker-binaries
//! step can give the worker its own crate without duplicating any of
//! this (the API temporarily keeps the libvips-linked dark path; see
//! the workspace-split notes in docs/backend.md).

pub mod config;
pub mod dbq;
pub mod error;
pub mod metrics;
pub mod store;
pub mod telemetry;
pub mod world;
