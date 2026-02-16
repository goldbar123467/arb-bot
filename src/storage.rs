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

/// Log reconciliation data for filled orders, matching them to brackets by ticker.
/// `incomplete` is true when the arb was only partially filled.
pub fn log_reconciliation(
    opp: &ArbOpportunity,
    filled_orders: &[(String, Order)],
    incomplete: bool,
) -> Result<()> {
    let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");

    let order_ids: Vec<&str> = filled_orders
        .iter()
        .map(|(_, o)| o.order_id.as_str())
        .collect();

    let statuses: Vec<&str> = filled_orders
        .iter()
        .map(|(_, o)| o.status.as_str())
        .collect();

    // Compute actual net profit from fill prices matched by ticker
    let mut actual_cost_or_revenue: i64 = 0;
    let mut actual_fees: i64 = 0;

    for (ticker, order) in filled_orders {
        let actual_price = order.yes_price.unwrap_or(0);
        let count = order.fill_count.or(order.count).unwrap_or(0) as u32;
        let fee = taker_fee_cents(count, actual_price);

        match opp.direction {
            ArbDirection::Long => {
                // Cost = price * count
                actual_cost_or_revenue += actual_price * count as i64;
            }
            ArbDirection::Short => {
                // Revenue = price * count
                actual_cost_or_revenue += actual_price * count as i64;
            }
        }
        actual_fees += fee;

        // Find expected price from brackets
        let expected_price = opp
            .brackets
            .iter()
            .find(|b| b.ticker == *ticker)
            .map(|b| match opp.direction {
                ArbDirection::Long => b.yes_ask_cents,
                ArbDirection::Short => b.yes_bid_cents,
            })
            .unwrap_or(0);

        if actual_price != expected_price {
            tracing::debug!(
                ticker = %ticker,
                expected = expected_price,
                actual = actual_price,
                "Price slippage detected"
            );
        }
    }

    // Use fill_count from first order as representative count, or fall back
    let position_size = filled_orders
        .first()
        .and_then(|(_, o)| o.fill_count.or(o.count))
        .unwrap_or(0);

    let actual_net = match opp.direction {
        ArbDirection::Long => {
            // Payout = 100 * position_size (one bracket pays), cost = actual_cost_or_revenue
            100 * position_size - actual_cost_or_revenue - actual_fees
        }
        ArbDirection::Short => {
            // Revenue = actual_cost_or_revenue, liability = 100 * position_size
            actual_cost_or_revenue - 100 * position_size - actual_fees
        }
    };

    let expected_net = opp.net_profit_cents;
    let slippage = actual_net - expected_net;

    let note = if incomplete { " (INCOMPLETE)" } else { "" };

    let line = format!(
        "| {} | {} | {} | {} | {} | ${:.2} | ${:.2} | ${:.2}{} |",
        ts,
        opp.event_ticker,
        opp.direction,
        order_ids.join(", "),
        statuses.join(", "),
        expected_net as f64 / 100.0,
        actual_net as f64 / 100.0,
        slippage as f64 / 100.0,
        note,
    );
    append_line("data/reconciliation.md", &line)
}
