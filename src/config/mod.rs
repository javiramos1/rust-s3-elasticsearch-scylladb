use color_eyre::Result;
use dotenv::dotenv;
use eyre::WrapErr;

use serde::Deserialize;
use tracing::{info, debug, instrument};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub schema_file: String,
    pub host: String,
    pub port: i32,
    pub region: String,
    pub db_url: String,
    pub es_url: String,
    pub es_enabled: bool,
    pub es_batch_size: usize,
    pub es_num_shards: usize,
    pub es_index: String,
    pub es_user: Option<String>,
    pub es_password: Option<String>,
    pub db_dc: String,
    pub parallel_files: usize,
    pub db_parallelism: usize,
    pub es_parallelism: usize,
    pub es_refresh_interval: String,
    pub es_source_enabled: bool
}

fn init_tracer() {
    #[cfg(debug_assertions)]
    let tracer = tracing_subscriber::fmt();
    #[cfg(not(debug_assertions))]
    let tracer = tracing_subscriber::fmt().json();

    tracer.with_env_filter(EnvFilter::from_default_env()).init();
}

impl Config {
    #[instrument]
    pub fn from_env() -> Result<Config> {
        dotenv().ok();

        init_tracer();

        info!("Loading configuration");

        let mut c = config::Config::new();

        c.merge(config::Environment::default())?;

        let config = c.try_into()
            .context("loading configuration from environment");

        debug!("Config: {:?}", config);
        config
    }
}
