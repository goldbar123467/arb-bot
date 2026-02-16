# bracket-arb

Automated bracket arbitrage scanner and executor for [Kalshi](https://kalshi.com) prediction markets.

Detects Dutch book arbitrage opportunities on mutually exclusive bracket events (e.g. "What will the high temperature in NYC be tomorrow?") where the sum of YES prices across all brackets deviates from $1.00 after fees.

## How it works

1. **Scan** — Fetches events and orderbooks for configured series on a 30-second loop
2. **Detect** — Evaluates both LONG (buy all YES) and SHORT (sell all YES) directions for each event
3. **Filter** — Checks net profit, ROI, and liquidity depth gates before signaling an opportunity
4. **Execute** — Places limit orders on all brackets concurrently, then handles partial fills and cancellations
5. **Log** — Records every scan, opportunity, trade, and reconciliation to append-only markdown files

### Arb detection

For a mutually exclusive bracket event with N outcomes:

- **LONG**: if `sum(YES_ask) < 100¢`, buy YES on every bracket. Guaranteed profit = `(100 - sum) * contracts - fees`
- **SHORT**: if `sum(YES_bid) > 100¢`, sell YES on every bracket. Guaranteed profit = `(sum - 100) * contracts - fees`

Fees use Kalshi's taker fee formula: `ceil(0.07 * C * P * (1-P) * 100) / 100` at 7 basis points.

## Project structure

```
src/
  main.rs           # Scan loop, series cache, risk limits, orchestration
  config.rs         # TOML config + env var loading
  detector.rs       # Arb detection, fee calculation, quote extraction
  executor.rs       # Concurrent order placement, fill classification
  storage.rs        # Append-only markdown logging (scans, opps, trades, reconciliation)
  telegram.rs       # Optional Telegram alerts for risk events and failures
  kalshi/
    client.rs       # HTTP client with read throttle + 429 retry/backoff
    auth.rs         # RSA-SHA256 request signing (Kalshi API auth)
    types.rs        # API response types + internal analysis types
tests/
  fixtures/         # Orderbook JSON fixtures for deserialization tests
config.toml         # Scanner, risk, and API configuration
```

## Setup

### Prerequisites

- Rust 1.70+
- Kalshi API key with RSA keypair ([docs](https://help.kalshi.com/faq/api))

### Configuration

1. Place your RSA private key at `secrets/kalshi_rsa.pem`

2. Create a `.env` file:
```env
KALSHI_API_KEY_ID=your-api-key-id
DRY_RUN=true
# Optional — Telegram alerts for risk limits, partial fills, failures
TELEGRAM_BOT_TOKEN=your-bot-token
TELEGRAM_CHAT_ID=your-chat-id
```

3. Edit `config.toml`:
```toml
[scanner]
interval_secs = 30
series_filter = [
  "KXHIGHNY", "KXHIGHMIA", "KXHIGHLAX",   # daily weather
  "KXCPI", "KXGDP", "KXPAYROLLS",          # economics
]
# scan_delay_ms = 150      # ms between API reads (default: 150)
# min_brackets = 2         # min active markets per event (default: 2)
# max_brackets = 15        # max active markets per event (default: 15)
# series_cache_secs = 300  # series list cache TTL (default: 300)

[risk]
min_net_profit_cents = 10   # $0.10 minimum net profit
min_roi_pct = 1.0           # 1% minimum ROI
position_size = 5           # contracts per bracket
max_open_positions = 5

[kalshi]
base_url = "https://api.elections.kalshi.com/trade-api/v2"
rsa_key_path = "secrets/kalshi_rsa.pem"
```

## Usage

```bash
# Build
cargo build --release

# Dry run (scan only, no orders)
DRY_RUN=true RUST_LOG=bracket_arb=debug cargo run

# Live
DRY_RUN=false RUST_LOG=bracket_arb=info ./target/release/bracket-arb

# Run in tmux (persists across SSH disconnects)
tmux new-session -d -s arb "./target/release/bracket-arb 2>&1 | tee arb.log"
tmux attach -t arb   # to monitor
```

## Rate limiting

The Kalshi Basic tier allows 20 reads/sec. The client enforces:

- **Read throttle**: configurable delay between GET requests (default 150ms = ~6.7 req/s)
- **429 retry**: parses `Retry-After` header, exponential backoff (1s/2s/4s), max 3 retries for reads, 2 for writes
- **Series cache**: caches the full series list for 5 minutes to avoid redundant pagination
- **Write passthrough**: POST/DELETE (order placement/cancellation) are not throttled — arb orders fire immediately

With 20 series and ~44 events, a scan cycle completes in ~40 seconds.

## Risk controls

Hardcoded circuit breakers (not configurable — these are safety nets):

| Limit | Value | Effect |
|-------|-------|--------|
| Max open arbs | 5 | Stops executing new arbs |
| Max daily loss | $5.00 | Halts all execution |
| Max daily orders | 50 | Halts all execution |

Additional safeguards:
- Mixed execution states (some brackets filled, some resting) trigger automatic cancellation of resting orders
- Worst-case loss from partial fills is tracked against daily P&L
- Telegram alerts fire on risk limit hits, partial fills, and total failures

## Data logging

All logs are written to `data/` as append-only markdown tables:

| File | Contents |
|------|----------|
| `scans.md` | Cycle stats: series/events scanned, opportunities found, trades executed |
| `opportunities.md` | Every detected opportunity with direction, sum, fees, net profit, ROI |
| `trades.md` | Individual order placements with price, size, fee, order ID, status |
| `reconciliation.md` | Post-fill analysis: expected vs actual profit, slippage detection |

## Tests

```bash
cargo test
```

20 tests covering:
- Orderbook deserialization (null sides, both sides, unsorted levels)
- Fee calculation (edge cases, various contract sizes)
- Arb detection (profitability, gate independence, sort invariance via proptest)
- Order construction (LONG/SHORT payloads, serialization, price selection)

## License

Private — not for redistribution.
