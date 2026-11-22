//! Sonar API access: discovery, HTTP client, routes and models.
//!
//! The `SonarApi` trait and its types are added in Phase 1, once the response
//! shapes are confirmed by `probe` output in `docs/api-samples/`.

pub mod client;
pub mod discovery;
pub mod models;
pub mod routes;
