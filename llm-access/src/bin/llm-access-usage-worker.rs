//! Standalone usage journal consumer.

use better_mimalloc_rs::MiMalloc;

const DEFAULT_LOG_FILTER: &str =
    "warn,llm_access=info,llm_access_store=info,llm_usage_journal=info";

#[global_allocator]
static GLOBAL_MIMALLOC: MiMalloc = MiMalloc;

#[cfg(any(feature = "duckdb-runtime", feature = "duckdb-bundled"))]
fn main() -> anyhow::Result<()> {
    llm_access::allocator::configure_process_allocator_for_low_rss();
    let _log_guards = static_flow_runtime::runtime_logging::init_runtime_logging(
        "llm-access-usage-worker",
        DEFAULT_LOG_FILTER,
    )?;
    run()
}

#[cfg(any(feature = "duckdb-runtime", feature = "duckdb-bundled"))]
fn run() -> anyhow::Result<()> {
    use std::{
        ffi::OsString,
        sync::{Arc, RwLock},
    };

    use anyhow::Context;
    use llm_access::{
        config::{resolve_request_cache_config, CliCommand},
        usage_worker::{router, ClusterUsageWorker, EdgeUsageWorker, UsageWorker},
    };
    use llm_access_core::store::AdminConfigStore;
    use llm_access_store::{
        duckdb::{DuckDbUsageConnectionConfig, DuckDbUsageRepository, TieredDuckDbUsageConfig},
        postgres::PostgresControlRepository,
    };

    let args = std::env::args_os().collect::<Vec<OsString>>();
    let explicit_bind = args.iter().any(|arg| arg == "--bind");
    let command = CliCommand::parse(args)?;
    let (bind_addr, storage) = match command {
        CliCommand::Init(storage) => {
            llm_access::bootstrap_storage(&storage)?;
            return Ok(());
        },
        CliCommand::Serve(config) => (config.bind_addr, config.storage),
    };
    llm_access::bootstrap_usage_worker_storage(&storage)?;
    let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let cluster_state = runtime
        .block_on(llm_access::cluster::ClusterRuntimeState::from_storage_config(&storage))?;
    let control: Arc<dyn AdminConfigStore> = runtime.block_on(async {
        let database_url =
            std::env::var(&storage.control_store.database_url_env).with_context(|| {
                format!("missing control database env `{}`", storage.control_store.database_url_env)
            })?;
        let request_cache = resolve_request_cache_config(&storage)?;
        Ok::<Arc<dyn AdminConfigStore>, anyhow::Error>(Arc::new(
            PostgresControlRepository::connect(&database_url, request_cache).await?,
        ) as Arc<dyn AdminConfigStore>)
    })?;
    let runtime_config = runtime.block_on(control.get_admin_runtime_config())?;
    let bind_addr = if explicit_bind {
        bind_addr
    } else {
        runtime_config
            .usage_query_bind_addr
            .parse()
            .unwrap_or(bind_addr)
    };
    let role = match cluster_state.as_ref() {
        Some(cluster_state) => runtime.block_on(cluster_state.runtime_role()),
        None => llm_access::cluster::NodeRuntimeRole::Primary,
    };
    let connection_config = Arc::new(RwLock::new(
        DuckDbUsageConnectionConfig::from_admin_runtime_config(&runtime_config),
    ));
    let worker = match role {
        llm_access::cluster::NodeRuntimeRole::Primary => {
            let duckdb = if let Some(tiered) = storage.duckdb_tiered.clone() {
                DuckDbUsageRepository::open_tiered_with_connection_config(
                    TieredDuckDbUsageConfig {
                        active_dir: tiered.active_dir,
                        archive_dir: tiered.archive_dir,
                        catalog_dir: tiered.catalog_dir,
                        rollover_bytes: tiered.rollover_bytes,
                        details_dir: tiered.details_dir,
                    },
                    Arc::clone(&connection_config),
                )?
            } else {
                DuckDbUsageRepository::open_path_with_connection_config(
                    storage.duckdb.clone(),
                    Arc::clone(&connection_config),
                )?
            };
            ClusterUsageWorker::Primary(
                UsageWorker::new_with_retention_days(
                    storage.usage_journal_dir.clone(),
                    Arc::new(duckdb),
                    runtime_config.usage_journal_consumer_lease_ms,
                    runtime_config.usage_analytics_retention_days,
                )?
                .with_cluster_state(cluster_state.clone()),
            )
        },
        llm_access::cluster::NodeRuntimeRole::EdgeSecondary
        | llm_access::cluster::NodeRuntimeRole::Degraded => {
            let cluster_state = cluster_state
                .clone()
                .context("edge usage worker requires cluster state")?;
            let source_node_id = storage
                .node_identity
                .as_ref()
                .map(|identity| identity.node_id.clone())
                .context("edge usage worker requires node identity")?;
            ClusterUsageWorker::Edge(EdgeUsageWorker::new(
                storage.usage_journal_dir.clone(),
                cluster_state,
                source_node_id,
                runtime_config.usage_journal_consumer_lease_ms,
            )?)
        },
    };
    let app = router(&worker);
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::error!("failed to create usage worker import runtime: {err:#}");
                return;
            },
        };
        if let Err(err) =
            runtime.block_on(run_forever_with_runtime_config(worker, control, connection_config))
        {
            tracing::error!("llm access usage worker import loop stopped: {err:#}");
        }
    });
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        axum::serve(listener, app.into_make_service()).await?;
        Ok(())
    })
}

#[cfg(any(feature = "duckdb-runtime", feature = "duckdb-bundled"))]
async fn run_forever_with_runtime_config(
    worker: llm_access::usage_worker::ClusterUsageWorker,
    control: std::sync::Arc<dyn llm_access_core::store::AdminConfigStore>,
    connection_config: std::sync::Arc<
        std::sync::RwLock<llm_access_store::duckdb::DuckDbUsageConnectionConfig>,
    >,
) -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    use llm_access_store::duckdb::DuckDbUsageConnectionConfig;

    const RUNTIME_CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
    const USAGE_ANALYTICS_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5 * 60);

    let mut last_config_refresh = None::<Instant>;
    let mut last_maintenance = None::<Instant>;
    loop {
        if last_config_refresh
            .map(|last| last.elapsed() >= RUNTIME_CONFIG_REFRESH_INTERVAL)
            .unwrap_or(true)
        {
            match control.get_admin_runtime_config().await {
                Ok(runtime_config) => {
                    if let Ok(mut current) = connection_config.write() {
                        *current =
                            DuckDbUsageConnectionConfig::from_admin_runtime_config(&runtime_config);
                    }
                    if let llm_access::usage_worker::ClusterUsageWorker::Primary(primary) = &worker
                    {
                        primary.set_usage_analytics_retention_days(
                            runtime_config.usage_analytics_retention_days,
                        );
                    }
                },
                Err(err) => tracing::warn!(
                    "failed to refresh llm access usage worker runtime config: {err:#}"
                ),
            }
            last_config_refresh = Some(Instant::now());
        }
        if let Err(err) = worker.run_one_cycle().await {
            worker.record_error(&err);
            return Err(err);
        }
        if let llm_access::usage_worker::ClusterUsageWorker::Primary(primary) = &worker {
            if last_maintenance
                .map(|last| last.elapsed() >= USAGE_ANALYTICS_MAINTENANCE_INTERVAL)
                .unwrap_or(true)
            {
                if let Err(err) = primary.run_maintenance(now_ms()).await {
                    tracing::warn!("llm access usage analytics maintenance failed: {err:#}");
                }
                last_maintenance = Some(Instant::now());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(any(feature = "duckdb-runtime", feature = "duckdb-bundled"))]
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(not(any(feature = "duckdb-runtime", feature = "duckdb-bundled")))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("llm-access-usage-worker requires duckdb-runtime")
}
