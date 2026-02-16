use anyhow::Result;
use tracing::{error, info, warn};

use crate::kalshi::client::KalshiClient;
use crate::kalshi::types::*;
use crate::storage;

/// Classify an order into its execution bucket.
#[derive(Debug)]
pub struct ExecutionResult {
    pub event_ticker: String,
    pub direction: ArbDirection,
    pub filled: Vec<(String, Order)>,
    pub resting: Vec<(String, Order)>,
    pub other: Vec<(String, Order)>,
    pub api_failures: Vec<String>,
}

impl ExecutionResult {
    /// All brackets filled immediately.
    pub fn is_fully_filled(&self) -> bool {
        self.resting.is_empty()
            && self.other.is_empty()
            && self.api_failures.is_empty()
            && !self.filled.is_empty()
    }

    /// Every bracket failed — nothing to cancel.
    pub fn is_total_failure(&self) -> bool {
        self.filled.is_empty() && self.resting.is_empty() && self.other.is_empty()
    }
}

/// Build a CreateOrderRequest from a bracket quote and arb direction.
pub fn build_order_request(
    bracket: &BracketQuote,
    direction: ArbDirection,
    position_size: u32,
) -> CreateOrderRequest {
    match direction {
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
    }
}

/// Execute a Dutch book arb by placing orders on all brackets concurrently.
/// Returns an ExecutionResult classifying each order by status.
/// Does NOT cancel resting orders — caller decides cancel policy.
pub async fn execute_arb(
    client: &KalshiClient,
    opp: &ArbOpportunity,
    position_size: u32,
) -> Result<ExecutionResult> {
    info!(
        event = %opp.event_ticker,
        direction = %opp.direction,
        brackets = opp.brackets.len(),
        net_profit_cents = opp.net_profit_cents,
        "Executing arb"
    );

    let mut handles = Vec::new();

    for bracket in &opp.brackets {
        let req = build_order_request(bracket, opp.direction, position_size);

        let ticker = bracket.ticker.clone();
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            let result = client.create_order(&req).await;
            (ticker, result)
        }));
    }

    let mut filled = Vec::new();
    let mut resting = Vec::new();
    let mut other = Vec::new();
    let mut api_failures = Vec::new();

    for handle in handles {
        match handle.await {
            Ok((ticker, result)) => match result {
                Ok(order) => {
                    info!(ticker = %ticker, order_id = %order.order_id, status = %order.status, "Order placed");
                    storage::log_trade(opp, &ticker, &order, position_size)
                        .unwrap_or_else(|e| warn!("Failed to log trade: {}", e));
                    match order.status.as_str() {
                        "executed" => filled.push((ticker, order)),
                        "resting" => resting.push((ticker, order)),
                        _ => other.push((ticker, order)),
                    }
                }
                Err(e) => {
                    error!(ticker = %ticker, error = %e, "Order failed");
                    api_failures.push(ticker);
                }
            },
            Err(e) => {
                error!("Task panicked: {}", e);
            }
        }
    }

    Ok(ExecutionResult {
        event_ticker: opp.event_ticker.clone(),
        direction: opp.direction,
        filled,
        resting,
        other,
        api_failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    fn make_bracket(ticker: &str, yes_ask: i64, yes_bid: i64) -> BracketQuote {
        BracketQuote {
            ticker: ticker.to_string(),
            title: format!("{} title", ticker),
            yes_ask_cents: yes_ask,
            yes_bid_cents: yes_bid,
            depth_at_no: 100,
            depth_at_yes: 100,
        }
    }

    #[test]
    fn test_build_order_long_payload() {
        let bracket = make_bracket("TICKER-A", 35, 20);
        let req = build_order_request(&bracket, ArbDirection::Long, 5);
        let val = to_value(&req).unwrap();
        assert_eq!(
            val,
            json!({
                "ticker": "TICKER-A",
                "action": "buy",
                "side": "yes",
                "type": "limit",
                "count": 5,
                "yes_price": 35,
                "no_price": null,
            })
        );
    }

    #[test]
    fn test_build_order_short_payload() {
        let bracket = make_bracket("TICKER-B", 35, 20);
        let req = build_order_request(&bracket, ArbDirection::Short, 3);
        let val = to_value(&req).unwrap();
        assert_eq!(
            val,
            json!({
                "ticker": "TICKER-B",
                "action": "sell",
                "side": "yes",
                "type": "limit",
                "count": 3,
                "yes_price": 20,
                "no_price": null,
            })
        );
    }

    #[test]
    fn test_long_uses_ask_not_bid() {
        let bracket = make_bracket("T", 42, 18);
        let req = build_order_request(&bracket, ArbDirection::Long, 1);
        assert_eq!(req.yes_price, Some(42), "Long must use yes_ask_cents");
        assert_ne!(req.yes_price, Some(18), "Long must NOT use yes_bid_cents");
    }

    #[test]
    fn test_short_uses_bid_not_ask() {
        let bracket = make_bracket("T", 42, 18);
        let req = build_order_request(&bracket, ArbDirection::Short, 1);
        assert_eq!(req.yes_price, Some(18), "Short must use yes_bid_cents");
        assert_ne!(req.yes_price, Some(42), "Short must NOT use yes_ask_cents");
    }

    #[test]
    fn test_order_type_serializes_as_type() {
        let bracket = make_bracket("T", 50, 50);
        let req = build_order_request(&bracket, ArbDirection::Long, 1);
        let val = to_value(&req).unwrap();
        assert!(val.get("type").is_some(), "JSON must have 'type' key");
        assert!(
            val.get("order_type").is_none(),
            "JSON must NOT have 'order_type' key"
        );
    }

    #[test]
    fn test_position_size_flows_through() {
        let bracket = make_bracket("T", 30, 20);
        for size in [1u32, 5, 100] {
            let req = build_order_request(&bracket, ArbDirection::Long, size);
            assert_eq!(req.count, size);
        }
    }

    #[test]
    fn test_no_price_always_null() {
        let bracket = make_bracket("T", 60, 40);
        let long = build_order_request(&bracket, ArbDirection::Long, 1);
        let short = build_order_request(&bracket, ArbDirection::Short, 1);
        assert_eq!(long.no_price, None, "Long no_price must be None");
        assert_eq!(short.no_price, None, "Short no_price must be None");
    }
}
