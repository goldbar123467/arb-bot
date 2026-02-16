use anyhow::{bail, Context, Result};
use reqwest::Client;
use std::sync::Arc;
use tracing::{debug, warn};

use super::auth::KalshiAuth;
use super::types::*;

#[derive(Clone)]
pub struct KalshiClient {
    http: Client,
    auth: Arc<KalshiAuth>,
    base_url: String,
}

impl KalshiClient {
    pub fn new(auth: KalshiAuth, base_url: String) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            http,
            auth: Arc::new(auth),
            base_url,
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let headers = self.auth.headers("GET", path)?;

        let mut req = self.http.get(&url);
        for (k, v) in &headers {
            req = req.header(k, v);
        }

        let resp = req.send().await.context("HTTP GET failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("GET {} returned {}: {}", path, status, body);
        }
        resp.json::<T>().await.context("Failed to parse response")
    }

    async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let headers = self.auth.headers("POST", path)?;

        let mut req = self.http.post(&url).json(body);
        for (k, v) in &headers {
            req = req.header(k, v);
        }

        let resp = req.send().await.context("HTTP POST failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("POST {} returned {}: {}", path, status, body);
        }
        resp.json::<T>().await.context("Failed to parse response")
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
                    "/events?series_ticker={}&with_nested_markets=true&status=active&cursor={}",
                    series_ticker, c
                ),
                None => format!(
                    "/events?series_ticker={}&with_nested_markets=true&status=active",
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
        let headers = self.auth.headers("DELETE", &path)?;

        let mut req = self.http.delete(&url);
        for (k, v) in &headers {
            req = req.header(k, v);
        }

        let resp = req.send().await.context("HTTP DELETE failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!("Cancel order {} returned {}: {}", order_id, status, body);
        }
        Ok(())
    }
}
