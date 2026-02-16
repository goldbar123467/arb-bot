mod config;
mod detector;
mod executor;
mod kalshi;
mod storage;

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

use config::Config;
use detector::{detect_arb, quote_from_orderbook};
use kalshi::auth::KalshiAuth;
use kalshi::client::KalshiClient;

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
    let client = KalshiClient::new(auth, config.kalshi.base_url.clone())?;

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
        "Starting bracket arb scanner"
    );

    let mut open_positions = 0u32;

    while running.load(Ordering::SeqCst) {
        match scan_cycle(&client, &config, dry_run, &mut open_positions).await {
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
    open_positions: &mut u32,
) -> Result<()> {
    info!("Starting scan cycle");

    let series_list = client.list_series().await?;

    let series_to_scan: Vec<_> = if config.scanner.series_filter.is_empty() {
        series_list
    } else {
        series_list
            .into_iter()
            .filter(|s| config.scanner.series_filter.contains(&s.ticker))
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

            // Gate: need at least 2 markets
            let active_markets: Vec<_> = event
                .markets
                .iter()
                .filter(|m| m.status == "active")
                .collect();

            if active_markets.len() < 2 {
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

                if *open_positions >= config.risk.max_open_positions {
                    warn!("Max open positions reached, skipping");
                    storage::log_opportunity(opp, false)
                        .unwrap_or_else(|e| warn!("Failed to log: {}", e));
                    continue;
                }

                // Execute
                storage::log_opportunity(opp, true)
                    .unwrap_or_else(|e| warn!("Failed to log opportunity: {}", e));

                match executor::execute_arb(client, opp, config.risk.position_size).await {
                    Ok(orders) => {
                        if orders.len() == opp.brackets.len() {
                            trades_count += orders.len();
                            *open_positions += 1;
                            info!(
                                event = %opp.event_ticker,
                                orders = orders.len(),
                                "All orders placed successfully"
                            );
                        } else {
                            warn!(
                                event = %opp.event_ticker,
                                placed = orders.len(),
                                expected = opp.brackets.len(),
                                "Partial fill — orders cancelled"
                            );
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
