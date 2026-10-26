//! Firecrawl v1 and v2 API protocol compatibility layer.
//! Dual mounts endpoints to provide 100% backward compatibility with official Firecrawl SDKs.

pub mod batch;
pub mod crawl;
pub mod map;
pub mod parse;
pub mod scrape;
pub mod search;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};

use crate::server::AppState;

/// Assemble Firecrawl v1 and v2 routes.
pub fn firecrawl_routes() -> Router<AppState> {
    Router::new()
        // Firecrawl v2 API endpoints
        .route("/v2/scrape", post(scrape::scrape))
        .route(
            "/v2/parse",
            post(parse::parse).layer(DefaultBodyLimit::max(parse::MAX_PARSE_BODY_BYTES)),
        )
        .route("/v2/batch/scrape", post(batch::batch_start))
        .route(
            "/v2/batch/scrape/{id}",
            get(batch::job_status).delete(batch::job_cancel),
        )
        .route("/v2/batch/scrape/{id}/errors", get(batch::job_errors))
        .route("/v2/map", post(map::map))
        .route("/v2/search", post(search::search))
        .route("/v2/extract", post(search::extract_start))
        .route("/v2/extract/{id}", get(search::extract_status))
        .route("/v2/crawl", post(crawl::crawl_start))
        .route("/v2/crawl/active", get(crawl::jobs_active))
        .route(
            "/v2/crawl/{id}",
            get(batch::job_status).delete(batch::job_cancel),
        )
        .route("/v2/crawl/{id}/errors", get(batch::job_errors))
        // Firecrawl v1 API endpoints (full backward compatibility with v1 Python & Node SDKs)
        .route("/v1/scrape", post(scrape::scrape))
        .route("/v1/batch/scrape", post(batch::batch_start))
        .route(
            "/v1/batch/scrape/{id}",
            get(batch::job_status).delete(batch::job_cancel),
        )
        .route("/v1/batch/scrape/{id}/errors", get(batch::job_errors))
        .route("/v1/map", post(map::map))
        .route("/v1/search", post(search::search))
        .route("/v1/extract", post(search::extract_start))
        .route("/v1/extract/{id}", get(search::extract_status))
        .route("/v1/crawl", post(crawl::crawl_start))
        .route("/v1/crawl/active", get(crawl::jobs_active))
        .route(
            "/v1/crawl/{id}",
            get(batch::job_status).delete(batch::job_cancel),
        )
        .route("/v1/crawl/{id}/errors", get(batch::job_errors))
}
