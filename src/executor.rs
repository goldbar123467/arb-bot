use anyhow::Result;
use tracing::{error, info, warn};

use crate::kalshi::client::KalshiClient;
use crate::kalshi::types::*;
use crate::storage;

/// Execute a Dutch book arb by placing orders on all brackets concurrently.
pub async fn execute_arb(
    client: &KalshiClient,
    opp: &ArbOpportunity,
    position_size: u32,
) -> Result<Vec<(String, Order)>> {
    info!(
        event = %opp.event_ticker,
        direction = %opp.direction,
        brackets = opp.brackets.len(),
        net_profit_cents = opp.net_profit_cents,
        "Executing arb"
    );

    let mut handles = Vec::new();

    for bracket in &opp.brackets {
        let req = match opp.direction {
            ArbDirection::Long => CreateOrderRequest {
                ticker: bracket.ticker.clone(),
                action: "buy".to_string(),
                side: "yes".to_string(),
                order_type: "limit".to_string(),
                count: position_size,
                yes_price: Some(bracket.yes_ask_cents),
                no_price: None,
            },
            ArbDirection::Short => CreateOrderRequest {
                ticker: bracket.ticker.clone(),
                action: "sell".to_string(),
                side: "yes".to_string(),
                order_type: "limit".to_string(),
                count: position_size,
                yes_price: Some(bracket.yes_bid_cents),
                no_price: None,
            },
        };

        let ticker = bracket.ticker.clone();
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            let result = client.create_order(&req).await;
            (ticker, result)
        }));
    }

    let mut orders = Vec::new();
    let mut failed = Vec::new();

    for handle in handles {
        match handle.await {
            Ok((ticker, result)) => match result {
                Ok(order) => {
                    info!(ticker = %ticker, order_id = %order.order_id, status = %order.status, "Order placed");
                    storage::log_trade(opp, &ticker, &order, position_size)
                        .unwrap_or_else(|e| warn!("Failed to log trade: {}", e));
                    orders.push((ticker, order));
                }
                Err(e) => {
                    error!(ticker = %ticker, error = %e, "Order failed");
                    failed.push(ticker);
                }
            },
            Err(e) => {
                error!("Task panicked: {}", e);
            }
        }
    }

    // If some orders failed, attempt to cancel the ones that succeeded
    if !failed.is_empty() && !orders.is_empty() {
        warn!(
            failed = ?failed,
            filled = orders.len(),
            "Partial fill — cancelling successful orders"
        );
        for (ticker, order) in &orders {
            if let Err(e) = client.cancel_order(&order.order_id).await {
                error!(ticker = %ticker, order_id = %order.order_id, error = %e, "Cancel failed");
            }
        }
    }

    Ok(orders)
}
