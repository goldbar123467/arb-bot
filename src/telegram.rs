use anyhow::Result;
use tracing::{debug, warn};

/// Send an alert message via Telegram Bot API.
/// Reads TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID from the environment.
/// If either is missing, silently returns Ok (opt-in alerting).
pub async fn send_alert(message: &str) -> Result<()> {
    let token = match std::env::var("TELEGRAM_BOT_TOKEN") {
        Ok(t) => t,
        Err(_) => {
            debug!("TELEGRAM_BOT_TOKEN not set, skipping alert");
            return Ok(());
        }
    };
    let chat_id = match std::env::var("TELEGRAM_CHAT_ID") {
        Ok(c) => c,
        Err(_) => {
            debug!("TELEGRAM_CHAT_ID not set, skipping alert");
            return Ok(());
        }
    };

    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": message,
        "parse_mode": "Markdown",
    });

    let resp = reqwest::Client::new().post(&url).json(&body).send().await;
    match resp {
        Ok(r) if r.status().is_success() => {
            debug!("Telegram alert sent");
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            warn!("Telegram API returned {}: {}", status, body);
        }
        Err(e) => {
            warn!("Telegram alert failed: {}", e);
        }
    }

    Ok(())
}
