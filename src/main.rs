mod config;
mod detector;
mod executor;
mod kalshi;
mod storage;
mod telegram;

use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration, Instant};
use tracing::{debug, error, info, warn};

use config::Config;
use detector::{detect_arb, quote_from_orderbook};
use kalshi::auth::KalshiAuth;
use kalshi::client::KalshiClient;
use kalshi::types::Series;

// --- Hardcoded risk limits (not config — these are circuit breakers) ---
const MAX_OPEN_ARBS: u32 = 5;
const MAX_DAILY_LOSS_CENTS: i64 = 500; // $5.00 — halt if daily P&L drops below -$5
const MAX_DAILY_ORDERS: u32 = 50;

struct RiskLimits {
    open_arbs: u32,
    daily_pnl_cents: i64,
    daily_orders: u32,
    today: chrono::NaiveDate,
}

impl RiskLimits {
    fn new() -> Self {
        Self {
            open_arbs: 0,
            daily_pnl_cents: 0,
            daily_orders: 0,
            today: Utc::now().date_naive(),
        }
    }

    /// Reset counters if the date has rolled over.
    fn maybe_reset_day(&mut self) {
        let now = Utc::now().date_naive();
        if now != self.today {
            info!(
                prev_day = %self.today,
                pnl_cents = self.daily_pnl_cents,
                orders = self.daily_orders,
                "Daily risk counters reset"
            );
            self.daily_pnl_cents = 0;
            self.daily_orders = 0;
            self.today = now;
        }
    }

    /// Returns Some("reason") if any limit blocks execution, None if clear.
    fn check(&mut self) -> Option<&'static str> {
        self.maybe_reset_day();
        if self.open_arbs >= MAX_OPEN_ARBS {
            return Some("MAX_OPEN_ARBS");
        }
        if self.daily_pnl_cents <= -(MAX_DAILY_LOSS_CENTS) {
            return Some("MAX_DAILY_LOSS");
        }
        if self.daily_orders >= MAX_DAILY_ORDERS {
            return Some("MAX_DAILY_ORDERS");
        }
        None
    }
}

struct SeriesCache {
    series: Vec<Series>,
    fetched_at: Option<Instant>,
    ttl: Duration,
}

impl SeriesCache {
    fn new(ttl_secs: u64) -> Self {
        Self {
            series: Vec::new(),
            fetched_at: None, // starts stale to force first fetch
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    fn is_stale(&self) -> bool {
        match self.fetched_at {
            None => true,
            Some(t) => t.elapsed() >= self.ttl,
        }
    }

    async fn get_or_refresh(&mut self, client: &KalshiClient) -> Result<&[Series]> {
        if self.is_stale() {
            match client.list_series().await {
                Ok(fresh) => {
                    info!(count = fresh.len(), "Refreshed series list");
                    self.series = fresh;
                    self.fetched_at = Some(Instant::now());
                }
                Err(e) => {
                    if self.series.is_empty() {
                        return Err(e.context("Failed to fetch series list (no cached data)"));
                    }
                    warn!(
                        error = %e,
                        cached_count = self.series.len(),
                        "Failed to refresh series list, using stale cache"
                    );
                }
            }
        } else {
            debug!(cached_count = self.series.len(), "Using cached series list");
        }
        Ok(&self.series)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bracket_arb=info".parse().unwrap()),
        )
        .init();

    let config = Config::load().context("Failed to load config")?;
    let api_key_id = config::api_key_id()?;
    let dry_run = config::is_dry_run();

    if dry_run {
        info!("DRY RUN mode — will scan but not place orders");
    }

    let auth = KalshiAuth::new(&config.kalshi.rsa_key_path, api_key_id)?;
    let client = KalshiClient::new(auth, config.kalshi.base_url.clone(), config.scanner.scan_delay_ms)?;

    // Graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutdown signal received");
        r.store(false, Ordering::SeqCst);
    });

    info!(
        interval_secs = config.scanner.interval_secs,
        position_size = config.risk.position_size,
        min_profit = config.risk.min_net_profit_cents,
        min_roi = config.risk.min_roi_pct,
        scan_delay_ms = config.scanner.scan_delay_ms,
        min_brackets = config.scanner.min_brackets,
        max_brackets = config.scanner.max_brackets,
        series_cache_secs = config.scanner.series_cache_secs,
        "Starting bracket arb scanner"
    );

    let mut limits = RiskLimits::new();
    let mut series_cache = SeriesCache::new(config.scanner.series_cache_secs);

    while running.load(Ordering::SeqCst) {
        match scan_cycle(&client, &config, dry_run, &mut limits, &mut series_cache).await {
            Ok(_) => {}
            Err(e) => error!("Scan cycle error: {:#}", e),
        }

        // Sleep with early exit on shutdown
        for _ in 0..config.scanner.interval_secs {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            sleep(Duration::from_secs(1)).await;
        }
    }

    info!("Shut down cleanly");
    Ok(())
}

async fn scan_cycle(
    client: &KalshiClient,
    config: &Config,
    dry_run: bool,
    limits: &mut RiskLimits,
    series_cache: &mut SeriesCache,
) -> Result<()> {
    info!("Starting scan cycle");

    let all_series = series_cache.get_or_refresh(client).await?;

    let series_to_scan: Vec<_> = if config.scanner.series_filter.is_empty() {
        all_series.to_vec()
    } else {
        all_series
            .iter()
            .filter(|s| config.scanner.series_filter.contains(&s.ticker))
            .cloned()
            .collect()
    };

    let series_count = series_to_scan.len();
    let mut events_count = 0usize;
    let mut opportunities_count = 0usize;
    let mut trades_count = 0usize;

    for series in &series_to_scan {
        let events = match client.get_events(&series.ticker).await {
            Ok(e) => e,
            Err(e) => {
                warn!(series = %series.ticker, error = %e, "Failed to fetch events");
                continue;
            }
        };

        for event in &events {
            // Gate: must be mutually exclusive
            if !event.mutually_exclusive {
                continue;
            }

            // Gate: need enough active markets (but not too many)
            let active_markets: Vec<_> = event
                .markets
                .iter()
                .filter(|m| m.status == "active")
                .collect();

            if active_markets.len() < config.scanner.min_brackets {
                debug!(
                    event = %event.event_ticker,
                    markets = active_markets.len(),
                    min = config.scanner.min_brackets,
                    "Skipping event: too few active markets"
                );
                continue;
            }
            if active_markets.len() > config.scanner.max_brackets {
                debug!(
                    event = %event.event_ticker,
                    markets = active_markets.len(),
                    max = config.scanner.max_brackets,
                    "Skipping event: too many active markets"
                );
                continue;
            }

            events_count += 1;

            // Fetch orderbooks for all markets in this event
            let mut quotes = Vec::new();
            let mut skip_event = false;

            for market in &active_markets {
                match client.get_orderbook(&market.ticker).await {
                    Ok(ob) => {
                        if let Some(quote) = quote_from_orderbook(
                            &market.ticker,
                            &market.title,
                            &ob,
                        ) {
                            quotes.push(quote);
                        } else {
                            // No NO bids → can't compute YES ask → skip this event
                            skip_event = true;
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(
                            market = %market.ticker,
                            error = %e,
                            "Failed to fetch orderbook"
                        );
                        skip_event = true;
                        break;
                    }
                }
            }

            if skip_event || quotes.len() != active_markets.len() {
                continue;
            }

            // Detect arb opportunities
            let opps = detect_arb(
                &event.event_ticker,
                &event.title,
                &quotes,
                config.risk.position_size,
                config.risk.min_net_profit_cents,
                config.risk.min_roi_pct,
            );

            for opp in &opps {
                opportunities_count += 1;
                info!(
                    event = %opp.event_ticker,
                    direction = %opp.direction,
                    brackets = opp.brackets.len(),
                    sum = format!("${:.2}", opp.sum_cents as f64 / 100.0),
                    fees = format!("${:.2}", opp.total_fees_cents as f64 / 100.0),
                    net_profit = format!("${:.2}", opp.net_profit_cents as f64 / 100.0),
                    roi = format!("{:.1}%", opp.roi_pct),
                    "ARB FOUND"
                );

                if dry_run {
                    storage::log_opportunity(opp, false)
                        .unwrap_or_else(|e| warn!("Failed to log opportunity: {}", e));
                    continue;
                }

                // --- Pre-flight risk checks (hardcoded circuit breakers) ---
                if let Some(reason) = limits.check() {
                    warn!(
                        event = %opp.event_ticker,
                        reason = reason,
                        open_arbs = limits.open_arbs,
                        daily_pnl_cents = limits.daily_pnl_cents,
                        daily_orders = limits.daily_orders,
                        "RISK LIMIT HIT — skipping execution"
                    );
                    storage::log_opportunity(opp, false)
                        .unwrap_or_else(|e| warn!("Failed to log: {}", e));
                    let msg = format!(
                        "*RISK LIMIT: {}*\nEvent: `{}`\nOpen arbs: {}/{}\nDaily P&L: ${:.2}\nDaily orders: {}/{}",
                        reason,
                        opp.event_ticker,
                        limits.open_arbs, MAX_OPEN_ARBS,
                        limits.daily_pnl_cents as f64 / 100.0,
                        limits.daily_orders, MAX_DAILY_ORDERS,
                    );
                    telegram::send_alert(&msg).await.unwrap_or_else(|e| {
                        warn!("Telegram alert failed: {}", e);
                    });
                    continue;
                }

                // Execute
                storage::log_opportunity(opp, true)
                    .unwrap_or_else(|e| warn!("Failed to log opportunity: {}", e));

                match executor::execute_arb(client, opp, config.risk.position_size).await {
                    Ok(result) => {
                        let order_count = result.filled.len() + result.resting.len() + result.other.len();
                        limits.daily_orders += order_count as u32;

                        if result.is_fully_filled() {
                            trades_count += result.filled.len();
                            limits.open_arbs += 1;
                            limits.daily_pnl_cents += opp.net_profit_cents;
                            info!(
                                event = %opp.event_ticker,
                                orders = result.filled.len(),
                                "All orders filled successfully"
                            );

                            // Reconciliation: match filled orders to brackets by ticker
                            storage::log_reconciliation(opp, &result.filled, false)
                                .unwrap_or_else(|e| warn!("Failed to log reconciliation: {}", e));
                        } else if result.is_total_failure() {
                            error!(
                                event = %opp.event_ticker,
                                api_failures = result.api_failures.len(),
                                "Total execution failure — no orders placed"
                            );
                            let msg = format!(
                                "*TOTAL FAILURE*\nEvent: `{}`\nDirection: {}\nBrackets: {}\nAll {} orders failed",
                                opp.event_ticker,
                                opp.direction,
                                opp.brackets.len(),
                                result.api_failures.len(),
                            );
                            telegram::send_alert(&msg).await.unwrap_or_else(|e| {
                                warn!("Telegram alert failed: {}", e);
                            });
                        } else {
                            // Mixed state: some filled, some resting/failed
                            // Worst-case loss: cost of filled orders (unhedged position)
                            let loss: i64 = result.filled.iter()
                                .map(|(_, o)| o.yes_price.unwrap_or(0) * o.count.unwrap_or(0))
                                .sum();
                            limits.daily_pnl_cents -= loss;

                            warn!(
                                event = %opp.event_ticker,
                                filled = result.filled.len(),
                                resting = result.resting.len(),
                                other = result.other.len(),
                                api_failures = result.api_failures.len(),
                                loss_cents = loss,
                                "Mixed execution state — cancelling resting orders"
                            );

                            // Cancel all resting orders
                            for (ticker, order) in &result.resting {
                                if let Err(e) = client.cancel_order(&order.order_id).await {
                                    error!(
                                        ticker = %ticker,
                                        order_id = %order.order_id,
                                        error = %e,
                                        "Cancel failed"
                                    );
                                }
                            }
                            // Cancel any other-status orders too
                            for (ticker, order) in &result.other {
                                if let Err(e) = client.cancel_order(&order.order_id).await {
                                    error!(
                                        ticker = %ticker,
                                        order_id = %order.order_id,
                                        error = %e,
                                        "Cancel failed"
                                    );
                                }
                            }

                            // Log reconciliation for whatever did fill (incomplete arb)
                            if !result.filled.is_empty() {
                                storage::log_reconciliation(opp, &result.filled, true)
                                    .unwrap_or_else(|e| warn!("Failed to log reconciliation: {}", e));
                            }

                            let msg = format!(
                                "*PARTIAL FILL*\nEvent: `{}`\nDirection: {}\nBrackets: {}\nFilled: {}\nResting: {} (cancelled)\nFailed: {}\nExpected profit: ${:.2}",
                                opp.event_ticker,
                                opp.direction,
                                opp.brackets.len(),
                                result.filled.len(),
                                result.resting.len(),
                                result.api_failures.len() + result.other.len(),
                                opp.net_profit_cents as f64 / 100.0,
                            );
                            telegram::send_alert(&msg).await.unwrap_or_else(|e| {
                                warn!("Telegram alert failed: {}", e);
                            });
                        }
                    }
                    Err(e) => {
                        error!(event = %opp.event_ticker, error = %e, "Execution failed");
                    }
                }
            }
        }
    }

    storage::log_scan(series_count, events_count, opportunities_count, trades_count)
        .unwrap_or_else(|e| warn!("Failed to log scan: {}", e));

    info!(
        series = series_count,
        events = events_count,
        opportunities = opportunities_count,
        trades = trades_count,
        "Scan cycle complete"
    );

    Ok(())
}
