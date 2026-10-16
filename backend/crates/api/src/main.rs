//! Binary entry point; all logic lives in the library crate.
#[tokio::main]
async fn main() {
    if let Err(e) = idol_appreciation_club_backend::run().await {
        tracing::error!(error = ?e, "backend terminated");
        std::process::exit(1);
    }
}
