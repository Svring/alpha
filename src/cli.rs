use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Parser)]
#[command(name = "alpha-rust")]
#[command(about = "WorldQuant Brain alpha workflow CLI", long_about = None)]
pub struct Cli {
    #[arg(
        long,
        env = "BRAIN_API_URL",
        default_value = "https://api.worldquantbrain.com"
    )]
    pub api_url: String,
    #[arg(long, env = "BRAIN_USERNAME")]
    pub username: Option<String>,
    #[arg(long, env = "BRAIN_PASSWORD")]
    pub password: Option<String>,
    #[arg(long, env = "ALPHA_USER_INFO_FILE", default_value = "user_info.txt")]
    pub user_info_file: String,
    #[arg(long, env = "ALPHA_RECORDS_DIR", default_value = "records")]
    pub records_dir: String,
    #[arg(long, env = "ALPHA_LOGS_DIR", default_value = "logs")]
    pub logs_dir: String,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// Hunt: generate and simulate first-order alpha expressions from a dataset
    Hunt(HuntArgs),
    /// Refine: expand promising hunt alphas into second-order variants and simulate
    Refine(RefineArgs),
    Check(CheckArgs),
    Submit(SubmitArgs),
    Datasets(ListArgs),
    Datafields(DatafieldsArgs),
}

impl Commands {
    pub fn name(&self) -> &'static str {
        match self {
            Commands::Hunt(_) => "hunt",
            Commands::Refine(_) => "refine",
            Commands::Check(_) => "check",
            Commands::Submit(_) => "submit",
            Commands::Datasets(_) => "datasets",
            Commands::Datafields(_) => "datafields",
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct ListArgs {
    #[arg(long, default_value = "EQUITY")]
    pub instrument_type: String,
    #[arg(long, default_value = "USA")]
    pub region: String,
    #[arg(long, default_value = "TOP3000")]
    pub universe: String,
    #[arg(long, default_value_t = 1)]
    pub delay: i32,
}

#[derive(Debug, Clone, Args)]
pub struct DatafieldsArgs {
    #[arg(long, default_value = "EQUITY")]
    pub instrument_type: String,
    #[arg(long, default_value = "USA")]
    pub region: String,
    #[arg(long, default_value = "TOP3000")]
    pub universe: String,
    #[arg(long, default_value_t = 1)]
    pub delay: i32,
    #[arg(long)]
    pub dataset_id: Option<String>,
    #[arg(long)]
    pub search: Option<String>,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum FieldSource {
    Dataset,
    File,
}

/// Hunt: first-order alpha discovery. Only --dataset-id is required; tag is auto-composed.
#[derive(Debug, Clone, Args)]
pub struct HuntArgs {
    /// Dataset ID (e.g. fundamental6, analyst4). Required.
    #[arg(long)]
    pub dataset_id: String,
    /// Region [default: USA]
    #[arg(long, default_value = "USA")]
    pub region: String,
    /// Universe [default: TOP3000]
    #[arg(long, default_value = "TOP3000")]
    pub universe: String,
    /// Delay [default: 1]
    #[arg(long, default_value_t = 1)]
    pub delay: i32,
    /// Decay [default: 6]
    #[arg(long, default_value_t = 6)]
    pub decay: i32,
    /// Neutralization [default: SUBINDUSTRY]
    #[arg(long, default_value = "SUBINDUSTRY")]
    pub neutralization: String,
    /// Concurrent simulations [default: 3]
    #[arg(long, default_value_t = 3)]
    pub concurrency: usize,
    #[arg(long, value_enum, default_value_t = FieldSource::Dataset)]
    pub field_source: FieldSource,
    /// Required when --field-source file
    #[arg(long)]
    pub fields_file: Option<String>,
}

/// Refine: second-order expansion from hunt alphas. Tag is auto-composed from hunt-tag.
#[derive(Debug, Clone, Args)]
pub struct RefineArgs {
    /// Hunt tag (e.g. fundamental6_usa_1step). Required.
    #[arg(long)]
    pub hunt_tag: String,
    /// Sharpe threshold for selecting hunt alphas [default: 0.75]
    #[arg(long, default_value_t = 0.75)]
    pub sharpe_threshold: f64,
    /// Fitness threshold for selecting hunt alphas [default: 0.5]
    #[arg(long, default_value_t = 0.5)]
    pub fitness_threshold: f64,
    /// Concurrent simulations [default: 3]
    #[arg(long, default_value_t = 3)]
    pub concurrency: usize,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CheckMode {
    User,
    Consultant,
}

#[derive(Debug, Clone, Args)]
pub struct CheckArgs {
    #[arg(long, value_enum, default_value_t = CheckMode::User)]
    pub mode: CheckMode,
    #[arg(long, default_value = "start_date.txt")]
    pub start_date_file: String,
    #[arg(long, default_value = "submitable_alpha.csv")]
    pub submitable_file: String,
    #[arg(long, default_value = "USA")]
    pub regions: String,
    #[arg(long, default_value_t = 0.7)]
    pub self_corr_threshold: f64,
    #[arg(long, default_value_t = 0.7)]
    pub prod_corr_threshold: f64,
    #[arg(long, default_value_t = 1.25)]
    pub user_sharpe_threshold: f64,
    #[arg(long, default_value_t = 1.58)]
    pub consultant_sharpe_threshold: f64,
}

#[derive(Debug, Clone, Args)]
pub struct SubmitArgs {
    #[arg(long, value_delimiter = ',')]
    pub ids: Vec<String>,
    #[arg(long)]
    pub from_csv: Option<String>,
}
