use anyhow::{Context, Result};
use base64::Engine;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::RsaPrivateKey;
use sha2::Sha256;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct KalshiAuth {
    signing_key: SigningKey<Sha256>,
    api_key_id: String,
}

impl KalshiAuth {
    pub fn new(pem_path: &Path, api_key_id: String) -> Result<Self> {
        let pem_content = std::fs::read_to_string(pem_path)
            .with_context(|| format!("Failed to read RSA key from {}", pem_path.display()))?;
        let private_key = RsaPrivateKey::from_pkcs1_pem(&pem_content)
            .context("Failed to parse RSA private key (PKCS#1 PEM)")?;
        let signing_key = SigningKey::<Sha256>::new(private_key);
        Ok(Self {
            signing_key,
            api_key_id,
        })
    }

    pub fn timestamp_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    pub fn sign(&self, timestamp_ms: u64, method: &str, path: &str) -> Result<String> {
        let message = format!("{}{}{}", timestamp_ms, method, path);
        let signature = self.signing_key.sign(message.as_bytes());
        Ok(base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()))
    }

    pub fn headers(
        &self,
        method: &str,
        path: &str,
    ) -> Result<Vec<(String, String)>> {
        let ts = Self::timestamp_ms();
        let sig = self.sign(ts, method, path)?;
        Ok(vec![
            ("KALSHI-ACCESS-KEY".to_string(), self.api_key_id.clone()),
            ("KALSHI-ACCESS-TIMESTAMP".to_string(), ts.to_string()),
            ("KALSHI-ACCESS-SIGNATURE".to_string(), sig),
        ])
    }
}
