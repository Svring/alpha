use std::{collections::HashMap, fs, path::Path, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use tracing::info;
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::sleep;

use crate::cli::Cli;

#[derive(Clone)]
pub struct BrainClient {
    pub api_url: String,
    pub records_dir: String,
    pub username: String,
    pub password: String,
    client: Client,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimulationSettings<'a> {
    #[serde(rename = "instrumentType")]
    pub instrument_type: &'a str,
    pub region: &'a str,
    pub universe: &'a str,
    pub delay: i32,
    pub decay: i32,
    pub neutralization: &'a str,
    pub truncation: f64,
    pub pasteurization: &'a str,
    #[serde(rename = "unitHandling")]
    pub unit_handling: &'a str,
    #[serde(rename = "nanHandling")]
    pub nan_handling: &'a str,
    pub language: &'a str,
    pub visualization: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimulationBody<'a> {
    pub r#type: &'a str,
    pub settings: SimulationSettings<'a>,
    pub regular: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlphaLite {
    pub id: String,
    pub tags: Vec<String>,
    pub regular: AlphaRegular,
    pub settings: AlphaSettings,
    pub is: AlphaIs,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlphaRegular {
    pub code: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlphaSettings {
    pub region: String,
    pub universe: String,
    pub delay: i32,
    pub decay: i32,
    pub neutralization: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlphaCheck {
    pub name: String,
    pub result: String,
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlphaIs {
    pub sharpe: f64,
    pub turnover: f64,
    #[serde(rename = "longCount")]
    pub long_count: i64,
    #[serde(rename = "shortCount")]
    pub short_count: i64,
    pub checks: Vec<AlphaCheck>,
}

#[derive(Debug, Deserialize)]
struct PagedResponse<T> {
    count: Option<usize>,
    results: Vec<T>,
}

impl BrainClient {
    pub fn from_cli(cli: &Cli) -> Result<Self> {
        let (username, password) = read_credentials(
            cli.username.clone(),
            cli.password.clone(),
            Path::new(&cli.user_info_file),
        )?;
        fs::create_dir_all(&cli.records_dir)
            .with_context(|| format!("create records dir {}", cli.records_dir))?;

        let mut headers = HeaderMap::new();
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        let client = Client::builder()
            .default_headers(headers)
            .cookie_store(true)
            .build()
            .context("build http client")?;

        Ok(Self {
            api_url: cli.api_url.clone(),
            records_dir: cli.records_dir.clone(),
            username,
            password,
            client,
        })
    }

    pub async fn authenticate(&self) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/authentication", self.api_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("authentication request failed")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("authentication failed: {}", body);
        }
        info!("Login successful");
        Ok(())
    }

    pub async fn get_json_with_retry(&self, url: &str) -> Result<Value> {
        loop {
            let resp = self.client.get(url).send().await?;
            if let Some(wait) = retry_after_secs(resp.headers()) {
                sleep(Duration::from_secs_f64(wait)).await;
                continue;
            }
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                bail!("GET {} failed {}: {}", url, status, body);
            }
            return Ok(resp.json::<Value>().await?);
        }
    }

    pub async fn get_datafields(
        &self,
        instrument_type: &str,
        region: &str,
        delay: i32,
        universe: &str,
        dataset_id: Option<&str>,
        search: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut offset = 0;
        let limit = 50;
        loop {
            let mut url = format!(
                "{}/data-fields?instrumentType={}&region={}&delay={}&universe={}&limit={}&offset={}",
                self.api_url, instrument_type, region, delay, universe, limit, offset
            );
            if let Some(ds) = dataset_id {
                url.push_str("&dataset.id=");
                url.push_str(ds);
            }
            if let Some(q) = search {
                url.push_str("&search=");
                url.push_str(q);
            }
            let page: PagedResponse<Value> = self
                .client
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let got = page.results.len();
            out.extend(page.results);
            if got < limit {
                break;
            }
            offset += limit;
            sleep(Duration::from_secs(5)).await;
        }
        Ok(out)
    }

    pub async fn get_datasets(
        &self,
        instrument_type: &str,
        region: &str,
        delay: i32,
        universe: &str,
    ) -> Result<Vec<Value>> {
        let url = format!(
            "{}/data-sets?instrumentType={}&region={}&delay={}&universe={}",
            self.api_url, instrument_type, region, delay, universe
        );
        let page: PagedResponse<Value> = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(page.results)
    }

    pub async fn submit_simulation(&self, body: &SimulationBody<'_>) -> Result<Option<String>> {
        let resp = self
            .client
            .post(format!("{}/simulations", self.api_url))
            .json(body)
            .send()
            .await?;
        if resp.status().is_success() {
            return Ok(resp
                .headers()
                .get("Location")
                .and_then(|v| v.to_str().ok())
                .map(ToOwned::to_owned));
        }
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if text.contains("SIMULATION_LIMIT_EXCEEDED") {
            return Ok(None);
        }
        bail!("simulation post failed {}: {}", status, text);
    }

    pub async fn poll_simulation_alpha(&self, progress_url: &str) -> Result<Option<String>> {
        loop {
            let resp = self.client.get(progress_url).send().await?;
            if let Some(wait) = retry_after_secs(resp.headers()) {
                sleep(Duration::from_secs_f64(wait)).await;
                continue;
            }
            let status = resp.status();
            let json = resp.json::<Value>().await?;
            if !status.is_success() {
                bail!("poll failed {}: {}", status, json);
            }
            return Ok(json
                .get("alpha")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned));
        }
    }

    pub async fn set_alpha_properties(
        &self,
        alpha_id: &str,
        name: Option<&str>,
        color: Option<&str>,
        tags: Option<Vec<String>>,
    ) -> Result<()> {
        let mut payload = serde_json::json!({
            "category": Value::Null,
            "regular": { "description": Value::Null }
        });
        if let Some(v) = name {
            payload["name"] = Value::String(v.to_string());
        }
        if let Some(v) = color {
            payload["color"] = Value::String(v.to_string());
        }
        if let Some(v) = tags {
            payload["tags"] = serde_json::to_value(v)?;
        }
        self.client
            .patch(format!("{}/alphas/{}", self.api_url, alpha_id))
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn list_user_alphas_url(&self, url: String) -> Result<(Vec<AlphaLite>, usize)> {
        let page: PagedResponse<AlphaLite> = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok((page.results, page.count.unwrap_or(0)))
    }

    pub async fn get_corr_records(&self, alpha_id: &str, kind: &str) -> Result<Vec<Value>> {
        let url = format!("{}/alphas/{}/correlations/{}", self.api_url, alpha_id, kind);
        let json = self.get_json_with_retry(&url).await?;
        Ok(json
            .get("records")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    pub async fn submit_alpha(&self, alpha_id: &str) -> Result<u16> {
        let submit_url = format!("{}/alphas/{}/submit", self.api_url, alpha_id);
        let mut attempts = 0;
        while attempts < 5 {
            attempts += 1;
            let resp = self.client.post(&submit_url).send().await?;
            match resp.status().as_u16() {
                201 | 400 => break,
                403 => return Ok(403),
                _ => sleep(Duration::from_secs(3)).await,
            }
        }

        loop {
            let resp = self.client.get(&submit_url).send().await?;
            let status = resp.status().as_u16();
            if status == 200 {
                if let Some(wait) = retry_after_secs(resp.headers()) {
                    sleep(Duration::from_secs_f64(wait)).await;
                    continue;
                }
                return Ok(200);
            }
            if status == 403 {
                return Ok(403);
            }
            if status == 404 {
                return Ok(404);
            }
            if status == 429 {
                sleep(Duration::from_secs(600)).await;
                return Ok(429);
            }
            return Ok(status);
        }
    }
}

fn read_credentials(
    cli_username: Option<String>,
    cli_password: Option<String>,
    user_info_file: &Path,
) -> Result<(String, String)> {
    if let (Some(u), Some(p)) = (cli_username, cli_password) {
        return Ok((u, p));
    }
    if let (Ok(u), Ok(p)) = (std::env::var("BRAIN_USERNAME"), std::env::var("BRAIN_PASSWORD")) {
        if !u.is_empty() && !p.is_empty() {
            return Ok((u, p));
        }
    }
    let content = fs::read_to_string(user_info_file)
        .with_context(|| format!("read credentials: set BRAIN_USERNAME/BRAIN_PASSWORD in .env or create {}", user_info_file.display()))?;
    let mut map = HashMap::new();
    for line in content.lines() {
        if let Some((k, v)) = line.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().trim_matches('\'').to_string());
        }
    }
    let username = map
        .get("username")
        .cloned()
        .ok_or_else(|| anyhow!("missing username; set BRAIN_USERNAME in .env or username in {}", user_info_file.display()))?;
    let password = map
        .get("password")
        .cloned()
        .ok_or_else(|| anyhow!("missing password; set BRAIN_PASSWORD in .env or password in {}", user_info_file.display()))?;
    Ok((username, password))
}

fn retry_after_secs(headers: &HeaderMap) -> Option<f64> {
    headers
        .get("Retry-After")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
}
