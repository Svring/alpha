mod brain;
mod cli;
mod expr;
mod log;
mod workflows;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};
use log::{init as init_logging, RunSummary};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let command_name = cli.command.name();

    let guard = init_logging(command_name, &cli.logs_dir, cli.command.log_subfolder())?;
    let app = match brain::BrainClient::from_cli(&cli) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("{}", e);
            guard.finish(&RunSummary::default())?;
            return Err(e);
        }
    };

    let summary = match &cli.command {
        Commands::Hunt(args) => workflows::run_hunt(&app, args).await?,
        Commands::Refine(args) => workflows::run_refine(&app, args).await?,
        Commands::Check(args) => workflows::run_check(&app, args).await?,
        Commands::Submit(args) => workflows::run_submit(&app, args).await?,
        Commands::Datasets(args) => {
            workflows::run_list_datasets(&app, args).await?;
            RunSummary::default()
        }
        Commands::Datafields(args) => {
            workflows::run_list_datafields(&app, args).await?;
            RunSummary::default()
        }
    };

    guard.finish(&summary)?;
    Ok(())
}
