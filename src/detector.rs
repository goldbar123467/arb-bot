use crate::kalshi::types::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

/// Kalshi taker fee rate: 7 basis points of notional (0.07 = 7%).
/// Source: https://kalshi.com/docs/kalshi-fee-schedule.pdf
pub const FEE_BPS: i64 = 7;

/// Calculate Kalshi taker fee in cents for a given number of contracts at a price in cents.
/// Formula: ceil(0.07 * C * P * (1-P) * 100) / 100, where P is in dollars.
/// In cents: fee_cents = ceil(FEE_BPS * C * price_cents * (100 - price_cents) / 10_000)
pub fn taker_fee_cents(contracts: u32, price_cents: i64) -> i64 {
    if price_cents <= 0 || price_cents >= 100 {
        return 0;
    }
    let numerator = FEE_BPS * contracts as i64 * price_cents * (100 - price_cents);
    // Ceiling division: (a + b - 1) / b
    (numerator + 9_999) / 10_000
}

/// Extract a BracketQuote from an orderbook.
/// YES ask = 100 - best NO bid (buying YES means taking the other side of NO).
/// YES bid = best YES bid (selling YES means hitting the YES bid).
pub fn quote_from_orderbook(
    ticker: &str,
    title: &str,
    orderbook: &Orderbook,
) -> Option<BracketQuote> {
    // Best NO bid = highest price in no[] (sort-safe)
    let best_no_price = orderbook.no.iter().map(|l| l.price).max()?;
    if orderbook.no.first().map(|f| f.price) != Some(best_no_price) {
        debug!(
            "NO orderbook not sorted descending: first={}, max={}",
            orderbook.no[0].price, best_no_price
        );
    }

    // Best YES bid = highest price in yes[] (sort-safe)
    let best_yes_price = orderbook.yes.iter().map(|l| l.price).max();
    if let Some(best) = best_yes_price {
        if orderbook.yes.first().map(|f| f.price) != Some(best) {
            debug!(
                "YES orderbook not sorted descending: first={}, max={}",
                orderbook.yes[0].price, best
            );
        }
    }

    let yes_ask_cents = 100 - best_no_price;
    let yes_bid_cents = best_yes_price.unwrap_or(0);
    // Sum quantities at the best price (handles duplicate price levels)
    let depth_at_no: i64 = orderbook.no.iter()
        .filter(|l| l.price == best_no_price)
        .map(|l| l.quantity)
        .sum();
    let depth_at_yes: i64 = best_yes_price
        .map(|p| orderbook.yes.iter()
            .filter(|l| l.price == p)
            .map(|l| l.quantity)
            .sum())
        .unwrap_or(0);

    Some(BracketQuote {
        ticker: ticker.to_string(),
        title: title.to_string(),
        yes_ask_cents,
        yes_bid_cents,
        depth_at_no,
        depth_at_yes,
    })
}

/// Detect Dutch book arbitrage across a set of bracket quotes.
/// Returns opportunities for both Long and Short directions if they pass the gates.
pub fn detect_arb(
    event_ticker: &str,
    event_title: &str,
    quotes: &[BracketQuote],
    position_size: u32,
    min_net_profit_cents: u32,
    min_roi_pct: f64,
) -> Vec<ArbOpportunity> {
    let mut opps = Vec::new();

    // --- Direction 1: Long (buy YES on every bracket) ---
    {
        let sum_cents: i64 = quotes.iter().map(|q| q.yes_ask_cents).sum();
        let total_fees: i64 = quotes
            .iter()
            .map(|q| taker_fee_cents(position_size, q.yes_ask_cents))
            .sum();
        let gross_per_contract = 100 - sum_cents;
        let gross_profit = gross_per_contract * position_size as i64;
        let net_profit = gross_profit - total_fees;
        let total_cost = sum_cents * position_size as i64 + total_fees;

        let min_depth = quotes.iter().map(|q| q.depth_at_no).min().unwrap_or(0);

        let roi = if total_cost > 0 {
            Decimal::from(net_profit * 100) / Decimal::from(total_cost)
        } else {
            dec!(0)
        };

        debug!(
            event = event_ticker,
            direction = "LONG",
            brackets = quotes.len(),
            sum_cents,
            total_fees,
            net_profit,
            roi = %roi,
            min_depth,
            "Evaluated long arb"
        );

        if net_profit >= min_net_profit_cents as i64
            && roi >= Decimal::try_from(min_roi_pct).unwrap_or(dec!(1))
            && min_depth >= position_size as i64
        {
            opps.push(ArbOpportunity {
                event_ticker: event_ticker.to_string(),
                event_title: event_title.to_string(),
                direction: ArbDirection::Long,
                brackets: quotes.to_vec(),
                sum_cents,
                total_fees_cents: total_fees,
                gross_profit_cents: gross_profit,
                net_profit_cents: net_profit,
                roi_pct: roi,
            });
        }
    }

    // --- Direction 2: Short (sell YES on every bracket) ---
    {
        let sum_cents: i64 = quotes.iter().map(|q| q.yes_bid_cents).sum();
        let total_fees: i64 = quotes
            .iter()
            .map(|q| taker_fee_cents(position_size, q.yes_bid_cents))
            .sum();
        let gross_per_contract = sum_cents - 100;
        let gross_profit = gross_per_contract * position_size as i64;
        let net_profit = gross_profit - total_fees;
        // For short, "cost" is the liability = 100 cents per contract
        let total_cost = 100 * position_size as i64;

        let min_depth = quotes.iter().map(|q| q.depth_at_yes).min().unwrap_or(0);

        let roi = if total_cost > 0 {
            Decimal::from(net_profit * 100) / Decimal::from(total_cost)
        } else {
            dec!(0)
        };

        debug!(
            event = event_ticker,
            direction = "SHORT",
            brackets = quotes.len(),
            sum_cents,
            total_fees,
            net_profit,
            roi = %roi,
            min_depth,
            "Evaluated short arb"
        );

        if net_profit >= min_net_profit_cents as i64
            && roi >= Decimal::try_from(min_roi_pct).unwrap_or(dec!(1))
            && min_depth >= position_size as i64
        {
            opps.push(ArbOpportunity {
                event_ticker: event_ticker.to_string(),
                event_title: event_title.to_string(),
                direction: ArbDirection::Short,
                brackets: quotes.to_vec(),
                sum_cents,
                total_fees_cents: total_fees,
                gross_profit_cents: gross_profit,
                net_profit_cents: net_profit,
                roi_pct: roi,
            });
        }
    }

    opps
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Verify the fundamental accounting identity on every ArbOpportunity:
    ///   payout - cost = gross_profit
    ///   net_profit = gross_profit - fees
    fn assert_arb_identity(opp: &ArbOpportunity, position_size: u32) {
        let ps = position_size as i64;
        match opp.direction {
            ArbDirection::Long => {
                // Payout = 100 * ps (one bracket pays out)
                // Cost = sum_cents * ps
                let expected_gross = (100 - opp.sum_cents) * ps;
                assert_eq!(opp.gross_profit_cents, expected_gross,
                    "LONG gross: payout({}*{}) - cost({}*{}) != {}",
                    100, ps, opp.sum_cents, ps, opp.gross_profit_cents);
            }
            ArbDirection::Short => {
                // Revenue = sum_cents * ps (sell YES on all brackets)
                // Liability = 100 * ps
                let expected_gross = (opp.sum_cents - 100) * ps;
                assert_eq!(opp.gross_profit_cents, expected_gross,
                    "SHORT gross: revenue({}*{}) - liability({}*{}) != {}",
                    opp.sum_cents, ps, 100, ps, opp.gross_profit_cents);
            }
        }
        // net = gross - fees (direction-independent)
        assert_eq!(opp.net_profit_cents, opp.gross_profit_cents - opp.total_fees_cents,
            "net({}) != gross({}) - fees({})",
            opp.net_profit_cents, opp.gross_profit_cents, opp.total_fees_cents);
    }

    #[test]
    fn test_taker_fee_at_2_contracts() {
        assert_eq!(taker_fee_cents(2, 5), 1);   // $0.01
        assert_eq!(taker_fee_cents(2, 10), 2);  // $0.02
        assert_eq!(taker_fee_cents(2, 50), 4);  // $0.04
    }

    #[test]
    fn test_taker_fee_at_5_contracts() {
        // 5 contracts at 5c:  7*5*5*95   = 16625,  ceil(16625/10000) = 2
        assert_eq!(taker_fee_cents(5, 5), 2);
        // 5 contracts at 10c: 7*5*10*90  = 31500,  ceil(31500/10000) = 4
        assert_eq!(taker_fee_cents(5, 10), 4);
        // 5 contracts at 20c: 7*5*20*80  = 56000,  ceil(56000/10000) = 6
        assert_eq!(taker_fee_cents(5, 20), 6);
        // 5 contracts at 25c: 7*5*25*75  = 65625,  ceil(65625/10000) = 7
        assert_eq!(taker_fee_cents(5, 25), 7);
        // 5 contracts at 33c: 7*5*33*67  = 77490,  ceil(77490/10000) = 8
        assert_eq!(taker_fee_cents(5, 33), 8);
        // 5 contracts at 50c: 7*5*50*50  = 87500,  ceil(87500/10000) = 9
        assert_eq!(taker_fee_cents(5, 50), 9);
    }

    #[test]
    fn test_taker_fee_edge_cases() {
        assert_eq!(taker_fee_cents(5, 0), 0);
        assert_eq!(taker_fee_cents(5, 100), 0);
        assert_eq!(taker_fee_cents(0, 50), 0);
    }

    #[test]
    fn test_long_arb_worked_example() {
        // 4 brackets: A=10c, B=25c, C=40c, D=20c (sum=95c)
        let quotes = vec![
            BracketQuote { ticker: "A".into(), title: "A".into(), yes_ask_cents: 10, yes_bid_cents: 0, depth_at_no: 10, depth_at_yes: 0 },
            BracketQuote { ticker: "B".into(), title: "B".into(), yes_ask_cents: 25, yes_bid_cents: 0, depth_at_no: 10, depth_at_yes: 0 },
            BracketQuote { ticker: "C".into(), title: "C".into(), yes_ask_cents: 40, yes_bid_cents: 0, depth_at_no: 10, depth_at_yes: 0 },
            BracketQuote { ticker: "D".into(), title: "D".into(), yes_ask_cents: 20, yes_bid_cents: 0, depth_at_no: 10, depth_at_yes: 0 },
        ];
        // Sum=95. Gross/contract=5c. Gross for 5=25c.
        // Fees at 5 contracts: fee(5,10)=4 + fee(5,25)=7 + fee(5,40)=9 + fee(5,20)=6 = 26c.
        // Net = 25 - 26 = -1c. Not profitable.
        let opps = detect_arb("TEST", "Test Event", &quotes, 5, 10, 1.0);
        assert!(opps.is_empty(), "Should not find arb when sum=95c after fees");
    }

    #[test]
    fn test_long_arb_profitable() {
        // 3 brackets: sum = 85c. Gross/contract = 15c. Gross for 5 = 75c.
        let quotes = vec![
            BracketQuote { ticker: "A".into(), title: "A".into(), yes_ask_cents: 20, yes_bid_cents: 0, depth_at_no: 10, depth_at_yes: 0 },
            BracketQuote { ticker: "B".into(), title: "B".into(), yes_ask_cents: 25, yes_bid_cents: 0, depth_at_no: 10, depth_at_yes: 0 },
            BracketQuote { ticker: "C".into(), title: "C".into(), yes_ask_cents: 40, yes_bid_cents: 0, depth_at_no: 10, depth_at_yes: 0 },
        ];
        // Fees at 5: fee(5,20)=6 + fee(5,25)=7 + fee(5,40)=9 = 22c.
        // Net = 75 - 22 = 53c. ROI = 53/(425+22) = 11.9%.
        let opps = detect_arb("TEST", "Test", &quotes, 5, 10, 1.0);
        assert_eq!(opps.len(), 1);
        assert_eq!(opps[0].direction, ArbDirection::Long);
        assert_eq!(opps[0].net_profit_cents, 53);
        assert_arb_identity(&opps[0], 5);
    }

    #[test]
    fn test_quote_from_orderbook_unsorted() {
        let orderbook = Orderbook {
            no: vec![
                PriceLevel { price: 30, quantity: 5 },
                PriceLevel { price: 50, quantity: 20 },
                PriceLevel { price: 40, quantity: 10 },
            ],
            yes: vec![
                PriceLevel { price: 10, quantity: 3 },
                PriceLevel { price: 25, quantity: 15 },
                PriceLevel { price: 20, quantity: 8 },
            ],
        };
        let q = quote_from_orderbook("T", "Test", &orderbook).unwrap();
        // Best NO bid = 50 → yes_ask = 100 - 50 = 50
        assert_eq!(q.yes_ask_cents, 50);
        assert_eq!(q.depth_at_no, 20);
        // Best YES bid = 25
        assert_eq!(q.yes_bid_cents, 25);
        assert_eq!(q.depth_at_yes, 15);
    }

    #[test]
    fn test_quote_from_orderbook_empty_vecs() {
        // Empty NO → None
        let ob1 = Orderbook {
            no: vec![],
            yes: vec![PriceLevel { price: 30, quantity: 10 }],
        };
        assert!(quote_from_orderbook("T", "Test", &ob1).is_none());

        // Empty YES → Some with depth_at_yes: 0
        let ob2 = Orderbook {
            no: vec![PriceLevel { price: 60, quantity: 5 }],
            yes: vec![],
        };
        let q = quote_from_orderbook("T", "Test", &ob2).unwrap();
        assert_eq!(q.yes_ask_cents, 40);
        assert_eq!(q.yes_bid_cents, 0);
        assert_eq!(q.depth_at_no, 5);
        assert_eq!(q.depth_at_yes, 0);

        // Both empty → None
        let ob3 = Orderbook {
            no: vec![],
            yes: vec![],
        };
        assert!(quote_from_orderbook("T", "Test", &ob3).is_none());
    }

    #[test]
    fn test_gate_independence_long() {
        // depth_at_no sufficient, depth_at_yes = 0 → LONG fires, SHORT blocked
        let quotes = vec![
            BracketQuote { ticker: "A".into(), title: "A".into(), yes_ask_cents: 20, yes_bid_cents: 60, depth_at_no: 10, depth_at_yes: 0 },
            BracketQuote { ticker: "B".into(), title: "B".into(), yes_ask_cents: 25, yes_bid_cents: 60, depth_at_no: 10, depth_at_yes: 0 },
            BracketQuote { ticker: "C".into(), title: "C".into(), yes_ask_cents: 40, yes_bid_cents: 60, depth_at_no: 10, depth_at_yes: 0 },
        ];
        let opps = detect_arb("TEST", "Test", &quotes, 5, 10, 1.0);
        assert!(opps.iter().any(|o| o.direction == ArbDirection::Long), "LONG should fire");
        assert!(!opps.iter().any(|o| o.direction == ArbDirection::Short), "SHORT should be blocked by depth_at_yes=0");
        for opp in &opps {
            assert_arb_identity(opp, 5);
        }
    }

    #[test]
    fn test_gate_independence_short() {
        // depth_at_yes sufficient, depth_at_no = 0 → SHORT fires (if profitable), LONG blocked
        // sum_yes_bids = 60+60+60 = 180. gross/contract = 180-100 = 80. gross = 400.
        // fees: fee(5,60)=9 * 3 = 27 (approx). net = 400-27 = 373.
        let quotes = vec![
            BracketQuote { ticker: "A".into(), title: "A".into(), yes_ask_cents: 20, yes_bid_cents: 60, depth_at_no: 0, depth_at_yes: 10 },
            BracketQuote { ticker: "B".into(), title: "B".into(), yes_ask_cents: 25, yes_bid_cents: 60, depth_at_no: 0, depth_at_yes: 10 },
            BracketQuote { ticker: "C".into(), title: "C".into(), yes_ask_cents: 40, yes_bid_cents: 60, depth_at_no: 0, depth_at_yes: 10 },
        ];
        let opps = detect_arb("TEST", "Test", &quotes, 5, 10, 1.0);
        assert!(opps.iter().any(|o| o.direction == ArbDirection::Short), "SHORT should fire");
        assert!(!opps.iter().any(|o| o.direction == ArbDirection::Long), "LONG should be blocked by depth_at_no=0");
        for opp in &opps {
            assert_arb_identity(opp, 5);
        }
    }

    proptest! {
        #[test]
        fn proptest_quote_sort_invariant(
            no_levels in prop::collection::vec(
                (1i64..=99, 1i64..=1000).prop_map(|(p, q)| PriceLevel { price: p, quantity: q }),
                1..=20usize
            ),
            yes_levels in prop::collection::vec(
                (1i64..=99, 1i64..=1000).prop_map(|(p, q)| PriceLevel { price: p, quantity: q }),
                0..=20usize
            ),
        ) {
            use rand::seq::SliceRandom;
            use rand::thread_rng;

            let ob_original = Orderbook {
                no: no_levels.clone(),
                yes: yes_levels.clone(),
            };

            let mut no_shuffled = no_levels;
            let mut yes_shuffled = yes_levels;
            no_shuffled.shuffle(&mut thread_rng());
            yes_shuffled.shuffle(&mut thread_rng());

            let ob_shuffled = Orderbook {
                no: no_shuffled,
                yes: yes_shuffled,
            };

            let q1 = quote_from_orderbook("T", "Test", &ob_original);
            let q2 = quote_from_orderbook("T", "Test", &ob_shuffled);
            prop_assert_eq!(q1, q2);
        }
    }
}
