use anyhow::{bail, Context, Result};
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};
use tracing::{debug, warn};

use super::auth::KalshiAuth;
use super::types::*;

#[derive(Clone)]
pub struct KalshiClient {
    http: Client,
    auth: Arc<KalshiAuth>,
    base_url: String,
    last_read: Arc<Mutex<Instant>>,
    read_delay: Duration,
}

impl KalshiClient {
    pub fn new(auth: KalshiAuth, base_url: String, read_delay_ms: u64) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            http,
            auth: Arc::new(auth),
            base_url,
            last_read: Arc::new(Mutex::new(Instant::now())),
            read_delay: Duration::from_millis(read_delay_ms),
        })
    }

    /// Enforce minimum delay between read (GET) requests.
    async fn throttle_read(&self) {
        let mut last = self.last_read.lock().await;
        let elapsed = last.elapsed();
        if elapsed < self.read_delay {
            let wait = self.read_delay - elapsed;
            debug!(wait_ms = wait.as_millis(), "Throttling read request");
            sleep(wait).await;
        }
        *last = Instant::now();
    }

    /// Log rate-limit related headers from the response at debug level.
    fn log_rate_limit_headers(resp: &reqwest::Response, method: &str, path: &str) {
        let headers_to_check = [
            "x-ratelimit-remaining",
            "x-ratelimit-limit",
            "x-ratelimit-reset",
            "retry-after",
            "ratelimit-remaining",
            "ratelimit-limit",
            "ratelimit-reset",
        ];
        for name in &headers_to_check {
            if let Some(val) = resp.headers().get(*name) {
                debug!(
                    header = *name,
                    value = ?val,
                    method = method,
                    path = path,
                    "Rate limit header"
                );
            }
        }
    }

    /// Parse the Retry-After header as seconds.
    fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
        resp.headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<f64>().ok())
            .map(|secs| Duration::from_secs_f64(secs))
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.throttle_read().await;

        let url = format!("{}{}", self.base_url, path);
        let max_retries = 3u32;

        for attempt in 0..=max_retries {
            let headers = self.auth.headers("GET", path)?;
            let mut req = self.http.get(&url);
            for (k, v) in &headers {
                req = req.header(k, v);
            }

            let resp = req.send().await.context("HTTP GET failed")?;
            let status = resp.status();

            Self::log_rate_limit_headers(&resp, "GET", path);

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if attempt == max_retries {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("GET {} rate limited after {} retries: {}", path, max_retries, body);
                }
                let wait = Self::parse_retry_after(&resp).unwrap_or_else(|| {
                    let base = Duration::from_secs(1 << attempt);
                    base.min(Duration::from_secs(10))
                });
                warn!(
                    path = path,
                    attempt = attempt + 1,
                    wait_ms = wait.as_millis(),
                    "Rate limited (429), backing off"
                );
                sleep(wait).await;
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                bail!("GET {} returned {}: {}", path, status, body);
            }
            return resp.json::<T>().await.context("Failed to parse response");
        }
        unreachable!()
    }

    async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let max_retries = 2u32;

        for attempt in 0..=max_retries {
            let headers = self.auth.headers("POST", path)?;
            let mut req = self.http.post(&url).json(body);
            for (k, v) in &headers {
                req = req.header(k, v);
            }

            let resp = req.send().await.context("HTTP POST failed")?;
            let status = resp.status();

            Self::log_rate_limit_headers(&resp, "POST", path);

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if attempt == max_retries {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("POST {} rate limited after {} retries: {}", path, max_retries, body);
                }
                let wait = Self::parse_retry_after(&resp).unwrap_or_else(|| {
                    let base = Duration::from_secs(1 << attempt);
                    base.min(Duration::from_secs(5))
                });
                warn!(
                    path = path,
                    attempt = attempt + 1,
                    wait_ms = wait.as_millis(),
                    "Rate limited (429) on POST, backing off"
                );
                sleep(wait).await;
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                bail!("POST {} returned {}: {}", path, status, body);
            }
            return resp.json::<T>().await.context("Failed to parse response");
        }
        unreachable!()
    }

    /// List all series, paginating through all results.
    pub async fn list_series(&self) -> Result<Vec<Series>> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let path = match &cursor {
                Some(c) => format!("/series?cursor={}", c),
                None => "/series".to_string(),
            };
            let resp: SeriesResponse = self.get(&path).await?;
            all.extend(resp.series);
            match resp.cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }
        debug!("Fetched {} series", all.len());
        Ok(all)
    }

    /// Get events for a series, with nested markets.
    pub async fn get_events(&self, series_ticker: &str) -> Result<Vec<Event>> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let path = match &cursor {
                Some(c) => format!(
                    "/events?series_ticker={}&with_nested_markets=true&status=open&cursor={}",
                    series_ticker, c
                ),
                None => format!(
                    "/events?series_ticker={}&with_nested_markets=true&status=open",
                    series_ticker
                ),
            };
            let resp: EventsResponse = self.get(&path).await?;
            all.extend(resp.events);
            match resp.cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }
        debug!("Fetched {} events for series {}", all.len(), series_ticker);
        Ok(all)
    }

    /// Get orderbook for a single market.
    pub async fn get_orderbook(&self, ticker: &str) -> Result<Orderbook> {
        let path = format!("/markets/{}/orderbook?depth=5", ticker);
        let resp: OrderbookResponse = self.get(&path).await?;
        Ok(resp.orderbook)
    }

    /// Place a limit order.
    pub async fn create_order(&self, req: &CreateOrderRequest) -> Result<Order> {
        let path = "/portfolio/orders";
        let resp: CreateOrderResponse = self.post(path, req).await?;
        Ok(resp.order)
    }

    /// Cancel an order by ID.
    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        let path = format!("/portfolio/orders/{}", order_id);
        let url = format!("{}{}", self.base_url, path);
        let max_retries = 2u32;

        for attempt in 0..=max_retries {
            let headers = self.auth.headers("DELETE", &path)?;
            let mut req = self.http.delete(&url);
            for (k, v) in &headers {
                req = req.header(k, v);
            }

            let resp = req.send().await.context("HTTP DELETE failed")?;
            let status = resp.status();

            Self::log_rate_limit_headers(&resp, "DELETE", &path);

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if attempt == max_retries {
                    warn!("Cancel order {} rate limited after {} retries", order_id, max_retries);
                    return Ok(());
                }
                let wait = Self::parse_retry_after(&resp).unwrap_or_else(|| {
                    let base = Duration::from_secs(1 << attempt);
                    base.min(Duration::from_secs(5))
                });
                warn!(
                    order_id = order_id,
                    attempt = attempt + 1,
                    wait_ms = wait.as_millis(),
                    "Rate limited (429) on DELETE, backing off"
                );
                sleep(wait).await;
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                warn!("Cancel order {} returned {}: {}", order_id, status, body);
            }
            return Ok(());
        }
        unreachable!()
    }
}
