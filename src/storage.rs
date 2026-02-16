use anyhow::{Context, Result};
use chrono::Utc;
use std::fs::OpenOptions;
use std::io::Write;

use crate::detector::taker_fee_cents;
use crate::kalshi::types::*;

fn append_line(path: &str, line: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to open {}", path))?;
    writeln!(file, "{}", line)?;
    Ok(())
}

pub fn log_opportunity(opp: &ArbOpportunity, executed: bool) -> Result<()> {
    let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!(
        "| {} | {} | {} | {} | ${:.2} | ${:.2} | ${:.2} | {:.1}% | {} |",
        ts,
        opp.event_ticker,
        opp.direction,
        opp.brackets.len(),
        opp.sum_cents as f64 / 100.0,
        opp.total_fees_cents as f64 / 100.0,
        opp.net_profit_cents as f64 / 100.0,
        opp.roi_pct,
        if executed { "YES" } else { "NO" },
    );
    append_line("data/opportunities.md", &line)
}

pub fn log_trade(
    opp: &ArbOpportunity,
    ticker: &str,
    order: &Order,
    position_size: u32,
) -> Result<()> {
    let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let price_cents = order.yes_price.unwrap_or(0);
    let fee = taker_fee_cents(position_size, price_cents);
    let side = match opp.direction {
        ArbDirection::Long => "BUY_YES",
        ArbDirection::Short => "SELL_YES",
    };
    let line = format!(
        "| {} | {} | {} | {} | ${:.2} | {} | ${:.2} | {} | {} |",
        ts,
        opp.event_ticker,
        ticker,
        side,
        price_cents as f64 / 100.0,
        position_size,
        fee as f64 / 100.0,
        order.order_id,
        order.status,
    );
    append_line("data/trades.md", &line)
}

pub fn log_scan(
    series_count: usize,
    events_count: usize,
    opportunities: usize,
    trades: usize,
) -> Result<()> {
    let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!(
        "| {} | {} | {} | {} | {} |",
        ts, series_count, events_count, opportunities, trades,
    );
    append_line("data/scans.md", &line)
}
