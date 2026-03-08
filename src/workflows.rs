use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use chrono::{Duration, NaiveDate, Utc};
use futures::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{Mutex, Semaphore},
    time::{Duration as TokioDuration, sleep},
};
use tracing::{info, warn};

use crate::{
    brain::{AlphaLite, BrainClient, SimulationBody, SimulationSettings},
    cli::{
        CheckArgs, CheckMode, DatafieldsArgs, HuntArgs, ListArgs, RefineArgs, SubmitArgs,
    },
    expr,
    log::RunSummary,
};

/// Compose hunt tag from dataset_id and region: e.g. fundamental6 + USA → fundamental6_usa_1step
pub fn compose_hunt_tag(dataset_id: &str, region: &str) -> String {
    let region_lower = region.to_lowercase();
    format!("{dataset_id}_{region_lower}_1step")
}

/// Compose refine tag from hunt tag: e.g. fundamental6_usa_1step → fundamental6_usa_2step
pub fn compose_refine_tag(hunt_tag: &str) -> String {
    hunt_tag.replace("_1step", "_2step")
}

pub async fn run_list_datasets(client: &BrainClient, args: &ListArgs) -> Result<()> {
    client.authenticate().await?;
    let rows = client
        .get_datasets(
            &args.instrument_type,
            &args.region,
            args.delay,
            &args.universe,
        )
        .await?;
    for row in rows {
        println!("{}", serde_json::to_string_pretty(&row)?);
    }
    Ok(())
}

pub async fn run_list_datafields(client: &BrainClient, args: &DatafieldsArgs) -> Result<()> {
    client.authenticate().await?;
    let rows = client
        .get_datafields(
            &args.instrument_type,
            &args.region,
            args.delay,
            &args.universe,
            args.dataset_id.as_deref(),
            args.search.as_deref(),
        )
        .await?;
    for row in rows {
        println!("{}", serde_json::to_string_pretty(&row)?);
    }
    Ok(())
}

pub async fn run_hunt(client: &BrainClient, args: &HuntArgs) -> Result<RunSummary> {
    client.authenticate().await?;
    let hunt_tag = compose_hunt_tag(&args.dataset_id, &args.region);
    let expression_file = records_path(client, &format!("{}_simulated_alpha_expression.txt", hunt_tag));
    let completed = read_lines_set(&expression_file).await?;

    let fields = match args.field_source {
        crate::cli::FieldSource::Dataset => {
            info!("Fetching dataset fields (5s between pages)...");
            let datafields = client
                .get_datafields(
                    "EQUITY",
                    &args.region,
                    args.delay,
                    &args.universe,
                    Some(&args.dataset_id),
                    None,
                )
                .await?;
            expr::process_datafields(&datafields)
        }
        crate::cli::FieldSource::File => {
            let p = args
                .fields_file
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("--fields-file is required when --field-source file"))?;
            read_fields_from_file(p).await?
        }
    };

    let mut expressions = expr::first_order_factory(&fields);
    expressions.retain(|e| !completed.contains(e));
    if expressions.is_empty() {
        info!("No new expressions to simulate");
        return Ok(RunSummary::default());
    }
    let count = expressions.len();
    info!(
        "Hunt {}: {} pending, {} already completed",
        hunt_tag,
        count,
        completed.len()
    );
    simulate_expression_batch(
        client,
        expressions,
        &hunt_tag,
        &args.region,
        &args.universe,
        args.delay,
        args.decay,
        &args.neutralization,
        args.concurrency,
        &expression_file,
    )
    .await?;
    Ok(RunSummary {
        expressions_simulated: Some(count),
        ..Default::default()
    })
}

pub async fn run_refine(client: &BrainClient, args: &RefineArgs) -> Result<RunSummary> {
    client.authenticate().await?;
    let refine_tag = compose_refine_tag(&args.hunt_tag);
    info!("Refine {} ← hunt {}", refine_tag, args.hunt_tag);

    let refine_file = records_path(client, &format!("{}_simulated_alpha_expression.txt", refine_tag));
    let completed = read_lines_set(&refine_file).await?;

    let (mut recs, region, universe, delay, _instrument_type, neutralization) =
        get_alphas_for_refine(client, &args.hunt_tag, args.sharpe_threshold, args.fitness_threshold).await?;

    if recs.is_empty() {
        warn!("No hunt alphas for tag {} (run hunt first or lower --sharpe-threshold)", args.hunt_tag);
        return Ok(RunSummary::default());
    }

    recs.sort_by(|a, b| b.sharpe.partial_cmp(&a.sharpe).unwrap_or(std::cmp::Ordering::Equal));
    info!("Config: region={}, universe={}, delay={}", region, universe, delay);

    let mut second: Vec<(String, i32)> = Vec::new();
    for r in recs {
        let mut base = r.code;
        if r.sharpe < 0.0 {
            base = format!("-{base}");
        }
        let decay = expr::adjusted_decay(r.turnover, r.decay).unwrap_or(r.decay);
        let group_exp = expr::second_order_group(&[base]);
        for g in group_exp {
            if !completed.contains(&g) {
                second.push((g, decay));
            }
        }
    }
    if second.is_empty() {
        info!("No new refine expressions to simulate");
        return Ok(RunSummary::default());
    }
    let count = second.len();
    info!("Refine: {} expressions to simulate", count);
    let exprs: Vec<String> = second.iter().map(|v| v.0.clone()).collect();
    let decays: Vec<i32> = second.iter().map(|v| v.1).collect();
    simulate_expression_batch_var_decay(
        client,
        exprs,
        decays,
        &refine_tag,
        &region,
        &universe,
        delay,
        &neutralization,
        args.concurrency,
        &refine_file,
    )
    .await?;
    Ok(RunSummary {
        expressions_simulated: Some(count),
        ..Default::default()
    })
}

pub async fn run_check(client: &BrainClient, args: &CheckArgs) -> Result<RunSummary> {
    client.authenticate().await?;
    let mode = args.mode.clone();
    let start_file = records_path(client, &args.start_date_file);
    let submit_csv = records_path(client, &args.submitable_file);
    let regions: Vec<String> = args
        .regions
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let mut submitable = Vec::new();
    let periods = generate_date_periods(&start_file, "2024-10-07").await?;
    for (start_date, end_date) in periods {
        for region in &regions {
            let sharpe = match mode {
                CheckMode::User => args.user_sharpe_threshold,
                CheckMode::Consultant => args.consultant_sharpe_threshold,
            };
            let to_check = get_alphas_for_submit(client, &start_date, &end_date, sharpe, region).await?;
            if to_check.is_empty() {
                continue;
            }
            info!("Check: {} alphas for region {}", to_check.len(), region);
            for alpha in to_check {
                check_single_alpha(
                    client,
                    &alpha,
                    &submit_csv,
                    mode.clone(),
                    args.self_corr_threshold,
                    args.prod_corr_threshold,
                    &mut submitable,
                )
                .await?;
            }
            if end_date < (Utc::now().date_naive() - Duration::days(3)).to_string() {
                fs::write(&start_file, format!("{end_date}\n")).await?;
            }
        }
    }
    Ok(RunSummary {
        alphas_submitable: if submitable.is_empty() { None } else { Some(submitable) },
        ..Default::default()
    })
}

pub async fn run_submit(client: &BrainClient, args: &SubmitArgs) -> Result<RunSummary> {
    client.authenticate().await?;
    let mut ids = args.ids.clone();
    if let Some(csv_path) = &args.from_csv {
        ids.extend(read_ids_from_csv(Path::new(csv_path)).await?);
    }
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        bail!("no alpha ids provided; use --ids or --from-csv");
    }
    let submitted = ids.clone();
    for id in ids {
        let status = client.submit_alpha(&id).await?;
        info!("Submit {} → {}", id, status);
    }
    Ok(RunSummary {
        alphas_submitted: Some(submitted),
        ..Default::default()
    })
}

#[derive(Debug, Clone)]
struct TrackAlpha {
    code: String,
    sharpe: f64,
    turnover: f64,
    decay: i32,
}

/// Fetch hunt alphas by tag (no date filter). Returns alphas plus config from first result.
async fn get_alphas_for_refine(
    client: &BrainClient,
    hunt_tag: &str,
    sharpe_th: f64,
    fitness_th: f64,
) -> Result<(
    Vec<TrackAlpha>,
    String,
    String,
    i32,
    String,
    String,
)> {
    let region = parse_region_from_hunt_tag(hunt_tag);
    let start_date = "2000-01-01";
    let end_date = "2100-12-31";
    let mut out = Vec::new();
    let mut offset = 0;
    let mut first_settings: Option<(String, String, i32, String)> = None;

    loop {
        let url = format!(
            "{}/users/self/alphas?limit=100&offset={offset}&tag%3D={hunt_tag}&is.longCount%3E=100&is.shortCount%3E=100&settings.region={region}&is.sharpe%3E={sharpe_th}&is.fitness%3E={fitness_th}&status=UNSUBMITTED&dateCreated%3E={start_date}T00:00:00-04:00&dateCreated%3C{end_date}T00:00:00-04:00&type=REGULAR&settings.instrumentType=EQUITY&order=-is.sharpe&hidden=false&type!=SUPER",
            client.api_url
        );
        let (rows, count) = client.list_user_alphas_url(url).await?;
        for r in &rows {
            if first_settings.is_none() {
                first_settings = Some((
                    r.settings.universe.clone(),
                    r.settings.region.clone(),
                    r.settings.delay,
                    r.settings
                        .neutralization
                        .clone()
                        .unwrap_or_else(|| "SUBINDUSTRY".to_string()),
                ));
            }
            if pass_track_checks(r, sharpe_th) {
                out.push(TrackAlpha {
                    code: r.regular.code.clone(),
                    sharpe: r.is.sharpe,
                    turnover: r.is.turnover,
                    decay: r.settings.decay,
                });
            }
        }
        offset += 100;
        if offset >= count || count == 0 {
            break;
        }
    }

    let (universe, region_used, delay, neutralization) = first_settings
        .unwrap_or_else(|| ("TOP3000".to_string(), region.clone(), 1, "SUBINDUSTRY".to_string()));

    Ok((out, region_used, universe, delay, "EQUITY".to_string(), neutralization))
}

fn parse_region_from_hunt_tag(hunt_tag: &str) -> String {
    let parts: Vec<&str> = hunt_tag.split('_').collect();
    if parts.len() >= 2 {
        parts[parts.len() - 2].to_uppercase()
    } else {
        "USA".to_string()
    }
}

fn pass_track_checks(alpha: &AlphaLite, sharpe_th: f64) -> bool {
    let mut map = HashMap::new();
    for c in &alpha.is.checks {
        map.insert(c.name.as_str(), c.value.unwrap_or(99.0));
    }
    let concentrated = *map.get("CONCENTRATED_WEIGHT").unwrap_or(&0.0);
    let sub_universe = *map.get("LOW_SUB_UNIVERSE_SHARPE").unwrap_or(&99.0);
    let two_year = *map.get("LOW_2Y_SHARPE").unwrap_or(&99.0);
    let ladder = *map.get("IS_LADDER_SHARPE").unwrap_or(&99.0);

    (alpha.is.long_count > 100 || alpha.is.short_count > 100)
        && concentrated < 0.2
        && sub_universe.abs() > sharpe_th / 1.66
        && two_year.abs() > sharpe_th
        && ladder.abs() > sharpe_th
        && !(alpha.settings.region == "CHN" && alpha.is.sharpe < 0.0)
}

async fn get_alphas_for_submit(
    client: &BrainClient,
    start_date: &str,
    end_date: &str,
    sharpe_th: f64,
    region: &str,
) -> Result<Vec<AlphaLite>> {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let url = format!(
            "{}/users/self/alphas?limit=100&offset={offset}&is.longCount%3E=10&is.shortCount%3E=10&settings.region={region}&is.sharpe%3E={sharpe_th}&is.fitness%3E=1&status=UNSUBMITTED&dateCreated%3E={start_date}T00:00:00-04:00&dateCreated%3C{end_date}T00:00:00-04:00&order=-is.sharpe&hidden=false&type!=SUPER&color!=RED",
            client.api_url
        );
        let (rows, count) = client.list_user_alphas_url(url).await?;
        out.extend(rows.into_iter().filter(|a| {
            !a.is.checks.iter().any(|c| c.result == "FAIL")
        }));
        offset += 100;
        if offset >= count || count == 0 {
            break;
        }
    }
    Ok(out)
}

async fn check_single_alpha(
    client: &BrainClient,
    alpha: &AlphaLite,
    submit_csv: &Path,
    mode: CheckMode,
    self_th: f64,
    prod_th: f64,
    submitable: &mut Vec<String>,
) -> Result<()> {
    let tag = alpha.tags.first().cloned().unwrap_or_default();
    let checked_file = records_path(client, &format!("{tag}_checked_alpha_id.txt"));
    let checked = read_lines_set(&checked_file).await?;
    if checked.contains(&alpha.id) {
        return Ok(());
    }

    let self_corr = self_corr_max(client, &alpha.id).await?;
    if self_corr >= self_th {
        append_line(&checked_file, &alpha.id).await?;
        client.set_alpha_properties(&alpha.id, None, Some("RED"), None).await?;
        return Ok(());
    }

    let mut prod_corr = 0.0;
    if matches!(mode, CheckMode::Consultant) {
        prod_corr = prod_corr_max(client, &alpha.id).await?;
        if prod_corr > prod_th {
            append_line(&checked_file, &alpha.id).await?;
            client.set_alpha_properties(&alpha.id, None, Some("RED"), None).await?;
            return Ok(());
        }
    }

    upsert_submitable(submit_csv, alpha, self_corr, prod_corr).await?;
    client
        .set_alpha_properties(&alpha.id, None, Some("GREEN"), None)
        .await?;
    submitable.push(alpha.id.clone());
    Ok(())
}

async fn self_corr_max(client: &BrainClient, alpha_id: &str) -> Result<f64> {
    let records = client.get_corr_records(alpha_id, "self").await?;
    Ok(records
        .iter()
        .filter_map(|r| r.get("correlation").and_then(|v| v.as_f64()))
        .fold(0.0_f64, f64::max))
}

async fn prod_corr_max(client: &BrainClient, alpha_id: &str) -> Result<f64> {
    let records = client.get_corr_records(alpha_id, "prod").await?;
    let mut maxv = 0.0;
    for r in records {
        let alphas = r.get("alphas").and_then(|v| v.as_i64()).unwrap_or(0);
        let val = r.get("max").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if alphas > 0 {
            maxv = f64::max(maxv, val);
        }
    }
    Ok(maxv)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct SubmitableRow {
    id: String,
    region: String,
    universe: String,
    delay: i32,
    decay: i32,
    code: String,
    self_corr: f64,
    prod_corr: f64,
}

async fn upsert_submitable(path: &Path, alpha: &AlphaLite, self_corr: f64, prod_corr: f64) -> Result<()> {
    let mut rows: HashMap<String, SubmitableRow> = HashMap::new();
    if path.exists() {
        let text = fs::read_to_string(path).await.unwrap_or_default();
        let mut rdr = csv::Reader::from_reader(text.as_bytes());
        for rec in rdr.deserialize::<SubmitableRow>().flatten() {
            rows.insert(rec.id.clone(), rec);
        }
    }
    rows.insert(
        alpha.id.clone(),
        SubmitableRow {
            id: alpha.id.clone(),
            region: alpha.settings.region.clone(),
            universe: alpha.settings.universe.clone(),
            delay: alpha.settings.delay,
            decay: alpha.settings.decay,
            code: alpha.regular.code.clone(),
            self_corr,
            prod_corr,
        },
    );
    let mut wtr = csv::Writer::from_writer(Vec::new());
    for v in rows.values() {
        wtr.serialize(v)?;
    }
    let bytes = wtr.into_inner()?;
    fs::write(path, bytes).await?;
    Ok(())
}

async fn simulate_expression_batch(
    client: &BrainClient,
    expressions: Vec<String>,
    tag: &str,
    region: &str,
    universe: &str,
    delay: i32,
    decay: i32,
    neutralization: &str,
    concurrency: usize,
    expression_file: &Path,
) -> Result<()> {
    let decays = vec![decay; expressions.len()];
    simulate_expression_batch_var_decay(
        client,
        expressions,
        decays,
        tag,
        region,
        universe,
        delay,
        neutralization,
        concurrency,
        expression_file,
    )
    .await
}

fn truncate_expr(s: &str, max_len: usize) -> String {
    let mut it = s.chars();
    let taken: String = it.by_ref().take(max_len).collect();
    if it.next().is_some() {
        format!("{}...", taken)
    } else {
        taken
    }
}

async fn simulate_expression_batch_var_decay(
    client: &BrainClient,
    expressions: Vec<String>,
    decays: Vec<i32>,
    tag: &str,
    region: &str,
    universe: &str,
    delay: i32,
    neutralization: &str,
    concurrency: usize,
    expression_file: &Path,
) -> Result<()> {
    let total = expressions.len();
    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let file_mutex = Arc::new(Mutex::new(()));
    let completed = Arc::new(AtomicUsize::new(0));
    let mut tasks = FuturesUnordered::new();
    for (exp, decay) in expressions.into_iter().zip(decays.into_iter()) {
        let c = client.clone();
        let t = tag.to_string();
        let r = region.to_string();
        let u = universe.to_string();
        let n = neutralization.to_string();
        let f = expression_file.to_path_buf();
        let semc = sem.clone();
        let file_lock = file_mutex.clone();
        let completed_c = Arc::clone(&completed);
        tasks.push(tokio::spawn(async move {
            let _permit = semc.acquire_owned().await?;
            let body = SimulationBody {
                r#type: "REGULAR",
                settings: SimulationSettings {
                    instrument_type: "EQUITY",
                    region: &r,
                    universe: &u,
                    delay,
                    decay,
                    neutralization: &n,
                    truncation: 0.08,
                    pasteurization: "ON",
                    unit_handling: "VERIFY",
                    nan_handling: "ON",
                    language: "FASTEXPR",
                    visualization: false,
                },
                regular: &exp,
            };

            let progress = loop {
                if let Some(url) = c.submit_simulation(&body).await? {
                    break url;
                }
                sleep(TokioDuration::from_secs(5)).await;
            };
            if let Some(alpha_id) = c.poll_simulation_alpha(&progress).await? {
                c.set_alpha_properties(&alpha_id, Some(&t), None, Some(vec![t.clone()]))
                    .await?;
                let _guard = file_lock.lock().await;
                append_line(&f, &exp).await?;
                let n = completed_c.fetch_add(1, Ordering::Relaxed) + 1;
                let preview = truncate_expr(&exp, 56);
                info!("Alpha {}/{}: {} {}", n, total, alpha_id, preview);
            }
            Result::<()>::Ok(())
        }));
    }
    while let Some(done) = tasks.next().await {
        done??;
    }
    Ok(())
}

fn records_path(client: &BrainClient, filename: &str) -> PathBuf {
    let mut p = PathBuf::from(&client.records_dir);
    p.push(filename);
    p
}

async fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    f.write_all(format!("{line}\n").as_bytes()).await?;
    Ok(())
}

async fn read_lines_set(path: &Path) -> Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let content = fs::read_to_string(path).await?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

async fn read_fields_from_file(path: &str) -> Result<Vec<String>> {
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("read fields file {}", path))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect())
}

async fn generate_date_periods(start_file: &Path, default_start: &str) -> Result<Vec<(String, String)>> {
    let start_str = if start_file.exists() {
        fs::read_to_string(start_file).await?.trim().to_string()
    } else {
        default_start.to_string()
    };
    let mut current = NaiveDate::parse_from_str(&start_str, "%Y-%m-%d")
        .with_context(|| format!("invalid date in {}", start_file.display()))?;
    let today = Utc::now().date_naive() + Duration::days(1);
    let mut out = Vec::new();
    while current < today {
        let next = current + Duration::days(1);
        out.push((current.to_string(), next.to_string()));
        current = next;
    }
    Ok(out)
}

async fn read_ids_from_csv(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).await?;
    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let headers = rdr.headers()?.clone();
    let idx = headers
        .iter()
        .position(|h| h == "id")
        .ok_or_else(|| anyhow::anyhow!("csv has no id column: {}", path.display()))?;
    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec?;
        if let Some(v) = rec.get(idx) {
            if !v.is_empty() {
                out.push(v.to_string());
            }
        }
    }
    Ok(out)
}

