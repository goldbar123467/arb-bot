use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};

// --- Series ---

#[derive(Debug, Deserialize)]
pub struct SeriesResponse {
    pub series: Vec<Series>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Series {
    pub ticker: String,
    pub title: String,
    pub status: Option<String>,
}

// --- Events ---

#[derive(Debug, Deserialize)]
pub struct EventsResponse {
    pub events: Vec<Event>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Event {
    pub event_ticker: String,
    pub title: String,
    pub mutually_exclusive: bool,
    pub status: Option<String>,
    #[serde(default)]
    pub markets: Vec<Market>,
}

// --- Markets ---

#[derive(Debug, Clone, Deserialize)]
pub struct Market {
    pub ticker: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub status: String,
    pub result: Option<String>,
}

// --- Orderbook ---

#[derive(Debug, Deserialize)]
pub struct OrderbookResponse {
    pub orderbook: Orderbook,
}

#[derive(Debug, Deserialize)]
pub struct Orderbook {
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub yes: Vec<PriceLevel>,
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub no: Vec<PriceLevel>,
}

/// Deserialize `null` as an empty Vec (Kalshi sends null when a side has no levels).
fn null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

/// A price level on the orderbook.
/// Kalshi returns each level as a JSON tuple `[price_cents, quantity]`.
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub struct PriceLevel {
    /// Price in cents (integer)
    pub price: i64,
    pub quantity: i64,
}

impl<'de> Deserialize<'de> for PriceLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (price, quantity) = <(i64, i64)>::deserialize(deserializer)?;
        Ok(PriceLevel { price, quantity })
    }
}

// --- Orders ---

#[derive(Debug, Serialize)]
pub struct CreateOrderRequest {
    pub ticker: String,
    pub action: String,     // "buy" or "sell"
    pub side: String,       // "yes" or "no"
    #[serde(rename = "type")]
    pub order_type: String, // "limit"
    pub count: u32,
    pub yes_price: Option<i64>,
    pub no_price: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrderResponse {
    pub order: Order,
}

#[derive(Debug, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub ticker: String,
    pub status: String,
    pub action: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String,
    pub yes_price: Option<i64>,
    pub no_price: Option<i64>,
    pub count: Option<i64>,
    pub remaining_count: Option<i64>,
}

// --- Bracket analysis types (internal, not API) ---

#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub struct BracketQuote {
    pub ticker: String,
    pub title: String,
    pub yes_ask_cents: i64,  // cost to buy YES = 100 - best_no_bid
    pub yes_bid_cents: i64,  // revenue from selling YES = best_yes_bid
    pub depth_at_no: i64,    // quantity at best NO bid (LONG depth gate)
    pub depth_at_yes: i64,   // quantity at best YES bid (SHORT depth gate)
}

#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    pub event_ticker: String,
    pub event_title: String,
    pub direction: ArbDirection,
    pub brackets: Vec<BracketQuote>,
    pub sum_cents: i64,
    pub total_fees_cents: i64,
    pub gross_profit_cents: i64,
    pub net_profit_cents: i64,
    pub roi_pct: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArbDirection {
    Long,  // Buy YES on every bracket
    Short, // Sell YES on every bracket
}

impl std::fmt::Display for ArbDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArbDirection::Long => write!(f, "LONG"),
            ArbDirection::Short => write!(f, "SHORT"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_orderbook_both_sides() {
        let json = include_str!("../../tests/fixtures/orderbook_both_sides.json");
        let resp: OrderbookResponse =
            serde_json::from_str(json).expect("fixture should deserialize");
        let ob = &resp.orderbook;

        // NO side: 2 levels
        assert_eq!(ob.no.len(), 2);
        assert_eq!(ob.no[0].price, 1);
        assert_eq!(ob.no[0].quantity, 5084);
        assert_eq!(ob.no[1].price, 2);
        assert_eq!(ob.no[1].quantity, 2839);

        // YES side: 5 levels
        assert_eq!(ob.yes.len(), 5);
        assert_eq!(ob.yes[0].price, 70);
        assert_eq!(ob.yes[0].quantity, 81);
        assert_eq!(ob.yes[4].price, 95);
        assert_eq!(ob.yes[4].quantity, 31);

        // All prices in valid Kalshi range (1-99 cents)
        for level in ob.no.iter().chain(ob.yes.iter()) {
            assert!(level.price >= 1 && level.price <= 99,
                "price {} out of range", level.price);
            assert!(level.quantity > 0,
                "quantity {} should be positive", level.quantity);
        }
    }

    #[test]
    fn test_deserialize_orderbook_null_yes() {
        let json = include_str!("../../tests/fixtures/orderbook_null_yes.json");
        let resp: OrderbookResponse =
            serde_json::from_str(json).expect("null-yes fixture should deserialize");
        let ob = &resp.orderbook;

        // NO side present
        assert_eq!(ob.no.len(), 1);
        assert_eq!(ob.no[0].price, 70);
        assert_eq!(ob.no[0].quantity, 100);

        // YES side is null → empty vec
        assert!(ob.yes.is_empty(), "null yes should deserialize as empty vec");
    }

    #[test]
    fn test_deserialize_orderbook_null_both() {
        let json = r#"{"orderbook":{"no":null,"yes":null}}"#;
        let resp: OrderbookResponse =
            serde_json::from_str(json).expect("null-both should deserialize");
        assert!(resp.orderbook.no.is_empty());
        assert!(resp.orderbook.yes.is_empty());
    }
}
