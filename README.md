# alpha-rust

Rust rewrite of the original Python `alpha` workflow with cleaner architecture, stronger CLI UX, and configuration via environment variables or command-line flags.

## What This Rebuild Covers

This project replicates the original core workflows:

- **`hunt`**: generate first-order alpha expressions from a dataset and simulate them (only `--dataset-id` required)
- **`refine`**: expand promising hunt alphas into second-order variants and simulate (config auto-replicated from hunt)
- **`check`**: scan unsubmitted alphas, run self/prod correlation gating, and produce `submitable_alpha.csv`
- **`submit`**: submit selected alpha IDs
- utility commands for `datasets` and `datafields`

## Improvements Over Python Version

- Single executable CLI with consistent options and help text
- Modular architecture (`cli`, `brain` API client, `expr` factory, `workflows`)
- Unified retry behavior for API polling and rate limits
- Configurable via env vars **and** CLI flags
- No hardcoded dataset/tag/region values in source code
- Safer records handling and CSV upsert behavior

## Requirements

- Rust stable (edition 2024)
- Valid WorldQuant Brain credentials

## Credentials and Configuration

**Preferred:** put credentials in a `.env` file in the project root (loaded automatically):

```env
BRAIN_USERNAME=you@example.com
BRAIN_PASSWORD=your_password
```

Resolution order: CLI `--username`/`--password` → `BRAIN_USERNAME`/`BRAIN_PASSWORD` from env (e.g. `.env`) → `user_info.txt` if present.

### Common environment variables

- `BRAIN_API_URL` (default: `https://api.worldquantbrain.com`)
- `ALPHA_USER_INFO_FILE` (default: `user_info.txt`; only used if `.env` / env credentials are not set)
- `ALPHA_RECORDS_DIR` (default: `records`)
- `ALPHA_LOGS_DIR` (default: `logs`)

## Build

```bash
cargo build --release
```

Run with:

```bash
cargo run -- <subcommand> [options]
```

## Docker

You don’t need to bake a specific run command: the image entrypoint is the binary, and you pass the subcommand when you run the container.

Build:

```bash
docker build -t alpha-rust .
```

Run (pass credentials via env and the command you want):

```bash
docker run --rm -it \
  -e BRAIN_USERNAME="$BRAIN_USERNAME" \
  -e BRAIN_PASSWORD="$BRAIN_PASSWORD" \
  -v "$(pwd)/records:/app/records" \
  -v "$(pwd)/logs:/app/logs" \
  alpha-rust hunt --dataset-id fundamental2
```

Example with a `.env` file (mount it and set `DOTENV_PATH` if your dotenvy version supports it, or rely on `-e`):

```bash
docker run --rm -it --env-file .env \
  -v "$(pwd)/records:/app/records" \
  -v "$(pwd)/logs:/app/logs" \
  alpha-rust refine --hunt-tag fundamental2_usa_1step
```

Mount `records` and `logs` so state and logs persist on the host. Any subcommand works: `hunt`, `refine`, `check`, `submit`, `datasets`, `datafields`.

## Command Usage

### 1) Hunt

Generate first-order expressions from a dataset and simulate. **Only `--dataset-id` is required**; the hunt tag is auto-composed as `{dataset_id}_{region}_1step` (e.g. `fundamental6` + `USA` → `fundamental6_usa_1step`).

**Minimal usage** (only `--dataset-id` required):

```bash
cargo run -- hunt --dataset-id fundamental6
```

**With overrides:**

```bash
cargo run -- hunt --dataset-id fundamental2
```

**Hunt defaults:**

| Option | Default |
|--------|---------|
| `--region` | USA |
| `--universe` | TOP3000 |
| `--delay` | 1 |
| `--decay` | 6 |
| `--neutralization` | SUBINDUSTRY |
| `--concurrency` | 3 |
| `--field-source` | dataset |

**Custom fields file** (instead of dataset scan):

```bash
cargo run -- hunt --dataset-id fundamental6 --field-source file --fields-file ./fields.txt
```

`fields.txt` should contain one expression/field per line.

**Data-fields rate limit:** When using `--field-source dataset`, the client fetches all data-fields from the Brain API in pages of 50. To avoid 429 Too Many Requests, there is a **5-second delay between each page** (same as the Python version). The first run on a large dataset can take several minutes before simulations start.

### 2) Refine

Expand promising hunt alphas into second-order grouped variants and simulate. **Only `--hunt-tag` is required**; the refine tag is auto-composed by replacing `_1step` with `_2step` (e.g. `fundamental6_usa_1step` → `fundamental6_usa_2step`). Region, universe, delay, and neutralization are **replicated from the hunt alphas**—no need to pass them. Refine covers **all** hunt-generated alphas regardless of date.

**Minimal usage:**

```bash
cargo run -- refine --hunt-tag fundamental6_usa_1step
```

**With overrides:**

```bash
cargo run -- refine --hunt-tag fundamental6_usa_1step --sharpe-threshold 0.8 --concurrency 5
```

**Refine defaults:**

| Option | Default |
|--------|---------|
| `--sharpe-threshold` | 0.75 |
| `--fitness-threshold` | 0.5 |
| `--concurrency` | 3 |

### 3) Check

Check candidates and create/update `submitable_alpha.csv`:

```bash
cargo run -- check \
  --mode user \
  --regions USA \
  --start-date-file start_date.txt \
  --submitable-file submitable_alpha.csv
```

Consultant mode (includes prod-correlation gating):

```bash
cargo run -- check --mode consultant --regions USA
```

### 4) Submit

Submit explicit IDs:

```bash
cargo run -- submit --ids VPdwWxw,AbCdEf1
```

Or submit IDs from CSV (must include `id` column):

```bash
cargo run -- submit --from-csv records/submitable_alpha.csv
```

### 5) Dataset/Datafield Discovery

```bash
cargo run -- datasets --region USA --universe TOP3000 --delay 1
cargo run -- datafields --dataset-id fundamental6 --region USA --universe TOP3000 --delay 1
```

## Records Directory

By default all artifacts are written under `records/`:

- `<hunt_tag>_simulated_alpha_expression.txt` (e.g. `fundamental6_usa_1step_simulated_alpha_expression.txt`)
- `<refine_tag>_simulated_alpha_expression.txt` (e.g. `fundamental6_usa_2step_simulated_alpha_expression.txt`)
- `<tag>_checked_alpha_id.txt`
- `start_date.txt`
- `submitable_alpha.csv`

Override with:

```bash
export ALPHA_RECORDS_DIR=/custom/path/to/records
```

## Logging

Runtime logs are written to date-based folders. **Same day = same folder**; different dates get different folders.

**Structure:**

```
logs/
  2025-03-07/
    hunt_14-30-22.log
    refine_15-45-10.log
    check_16-20-00.log
  2025-03-08/
    hunt_09-00-00.log
```

Each log file includes:

- Header: `=== {command} started at {timestamp} ===`
- All `info!` / `warn!` output (tee'd to stdout and file)
- Footer: command, execution time, expressions simulated, alphas submitable/submitted

Override the log directory:

```bash
export ALPHA_LOGS_DIR=/custom/path/to/logs
```

Log level defaults to `info` if `RUST_LOG` is not set. To reduce output: `RUST_LOG=warn cargo run -- hunt ...`

## Architecture

- `src/main.rs`: CLI entrypoint and command dispatch
- `src/cli.rs`: command and flag definitions
- `src/brain.rs`: Brain API client, auth, simulation, check/submit helpers
- `src/expr.rs`: expression factory logic (first-order and second-order)
- `src/workflows.rs`: workflow orchestration for hunt/refine/check/submit

## Notes

- This tool preserves the original workflow intent but restructures it for maintainability and automation.
- **Data-fields API:** When fetching fields from a dataset, the client waits 5 seconds between each page (50 fields) to avoid 429 rate limits; large datasets take longer to load before simulations start.
- Correlation checks use Brain correlation endpoints for reliability and speed.
- Always test on a small subset first before full-scale simulation/submission.
