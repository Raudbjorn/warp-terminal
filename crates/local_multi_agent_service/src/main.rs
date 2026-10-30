use anyhow::Context;
use local_multi_agent_service::{Config, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    local_multi_agent_service::install_tls_provider();
    local_multi_agent_service::load_dotenv(".env")?;
    let config = Config::from_env().context("failed to load local multi-agent service config")?;
    run(config).await
}
