use anyhow::Context;
use local_multi_agent_service::{Config, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    local_multi_agent_service::install_tls_provider();
    local_multi_agent_service::load_dotenv(".env")?;
    // The app starts this service with its network policy in the environment (local-only mode and
    // the private hosts the user allowed); without it the service would not enforce any. A
    // standalone service can set the same variables in `.env`, so this follows `load_dotenv`,
    // which leaves variables the parent or the shell already set alone.
    network_policy::init_from_env().context("invalid network policy in the environment")?;
    let config = Config::from_env().context("failed to load local multi-agent service config")?;
    run(config).await
}
