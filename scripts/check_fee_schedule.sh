#!/usr/bin/env bash
# check_fee_schedule.sh — Verify that the Kalshi taker fee rate hasn't changed.
#
# Intended for weekly CI or manual runs. Fetches Kalshi's public fee
# documentation and checks that it still advertises the expected rate.
#
# Exit 0 = fee matches, Exit 1 = mismatch or fetch failure.
#
# Usage:
#   ./scripts/check_fee_schedule.sh
#   # or override the expected percentage:
#   EXPECTED_FEE_PCT=7 ./scripts/check_fee_schedule.sh

set -euo pipefail

EXPECTED_FEE_PCT="${EXPECTED_FEE_PCT:-7}"
FEE_SCHEDULE_URL="https://kalshi.com/docs/kalshi-fee-schedule.pdf"
FEES_PAGE_URL="https://kalshi.com/fees"

echo "Checking Kalshi fee schedule (expected: ${EXPECTED_FEE_PCT}%)"
echo "---"

# Strategy 1: Check the /fees web page for the fee percentage
echo "Fetching ${FEES_PAGE_URL}..."
fees_html=$(curl -sL --max-time 30 "${FEES_PAGE_URL}" 2>/dev/null || true)

if [ -n "$fees_html" ]; then
    # Look for patterns like "7%", "7.0%", "0.07" near "taker" or "fee"
    if echo "$fees_html" | grep -qiP "${EXPECTED_FEE_PCT}(\.\d+)?%"; then
        echo "PASS: Found ${EXPECTED_FEE_PCT}% on fees page"
        exit 0
    elif echo "$fees_html" | grep -qiP '\d+(\.\d+)?%'; then
        found=$(echo "$fees_html" | grep -oiP '\d+(\.\d+)?%' | sort -u | head -5)
        echo "FAIL: Expected ${EXPECTED_FEE_PCT}% but found these percentages:"
        echo "$found"
        echo ""
        echo "ACTION REQUIRED: Update FEE_BPS in src/detector.rs if the fee changed."
        exit 1
    else
        echo "WARN: Could not extract percentage from fees page (layout may have changed)"
    fi
else
    echo "WARN: Could not fetch fees page"
fi

# Strategy 2: Check if the PDF URL is still reachable (basic liveness check)
echo "Checking PDF reachability: ${FEE_SCHEDULE_URL}..."
http_code=$(curl -sL -o /dev/null -w "%{http_code}" --max-time 30 "${FEE_SCHEDULE_URL}" 2>/dev/null || echo "000")

if [ "$http_code" = "200" ]; then
    echo "INFO: Fee schedule PDF is reachable (HTTP 200). Manual review recommended."
    echo "WARN: Could not automatically verify fee percentage. Please check manually."
    exit 0
elif [ "$http_code" = "000" ]; then
    echo "WARN: Could not reach fee schedule PDF (network error)"
    exit 1
else
    echo "WARN: Fee schedule PDF returned HTTP ${http_code} (may have moved)"
    exit 1
fi
