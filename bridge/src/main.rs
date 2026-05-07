#![recursion_limit = "256"]

use rs_matter::error::Error;

#[tokio::main]
async fn main() -> Result<(), Error> {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );

    pnp_bridge::run_bridge().await
}
