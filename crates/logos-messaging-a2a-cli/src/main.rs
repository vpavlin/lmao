mod agent;
mod cli;
mod common;
mod completion;
mod health;
mod info;
mod metrics;
mod presence;
mod session;
mod task;

use anyhow::Result;
use clap::Parser;
use logos_messaging_a2a_transport::Transport;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Commands, TransportKind};
use common::IdentityConfig;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let json = cli.json;
    let identity = IdentityConfig {
        keyfile: cli.keyfile.clone(),
        encrypt: cli.encrypt,
    };

    let transport: Arc<dyn Transport> = build_transport(&cli).await?;

    match cli.command {
        Commands::Agent { action } => agent::handle(action, transport, &identity, json).await,
        Commands::Task { action } => task::handle(action, transport, &identity, json).await,
        Commands::Presence { action } => presence::handle(action, transport, &identity, json).await,
        Commands::Session { action } => session::handle(action, transport, &identity, json).await,
        Commands::Health => health::handle(&cli.waku, json).await,
        Commands::Metrics => metrics::handle(transport, &identity, json).await,
        Commands::Completion { shell } => {
            completion::handle(shell);
            Ok(())
        }
        Commands::Info => info::handle(transport, &identity, json),
    }
}

/// Construct the chosen transport, boxed as `Arc<dyn Transport>` so all
/// command handlers can share a single signature.
async fn build_transport(cli: &Cli) -> Result<Arc<dyn Transport>> {
    match cli.transport {
        #[cfg(feature = "logos-delivery")]
        TransportKind::LogosDelivery => {
            use logos_messaging_a2a_transport::logos_delivery::{
                LogosDeliveryTransport, NodeConfig,
            };
            let mut config = NodeConfig::logos_dev();
            config.preset = Some(cli.preset.clone());
            let t = LogosDeliveryTransport::new(config).await?;
            Ok(Arc::new(t))
        }
        #[cfg(feature = "rest")]
        TransportKind::Rest => {
            use logos_messaging_a2a_transport::nwaku_rest::LogosMessagingTransport;
            Ok(Arc::new(LogosMessagingTransport::new(&cli.waku)))
        }
    }
}
