//! AWS SNS Publish — native SigV4 signed request.
//!
//! Credentials come from the environment (AWS_ACCESS_KEY_ID /
//! AWS_SECRET_ACCESS_KEY / optional AWS_SESSION_TOKEN) when set, falling
//! back to the values from dashusb.conf. The fallback matters: systemd
//! starts the server without sourcing the conf, so env-only lookups left
//! SNS permanently broken for installs configured through the web UI.
//! Region is parsed from the topic ARN, then the conf, then AWS_REGION.

use anyhow::{bail, Result};
use chrono::Utc;
use reqwest::Client;
use ring::hmac;

fn env_or_conf(env_key: &str, conf_val: &str) -> Option<String> {
    std::env::var(env_key)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| (!conf_val.is_empty()).then(|| conf_val.to_string()))
}

#[allow(clippy::too_many_arguments)]
pub async fn send(
    client: &Client,
    topic_arn: &str,
    region_conf: &str,
    access_key_conf: &str,
    secret_key_conf: &str,
    title: &str,
    message: &str,
) -> Result<()> {
    let access_key = env_or_conf("AWS_ACCESS_KEY_ID", access_key_conf)
        .ok_or_else(|| anyhow::anyhow!("AWS_ACCESS_KEY_ID not set (env or dashusb.conf)"))?;
    let secret_key = env_or_conf("AWS_SECRET_ACCESS_KEY", secret_key_conf)
        .ok_or_else(|| anyhow::anyhow!("AWS_SECRET_ACCESS_KEY not set (env or dashusb.conf)"))?;
    let session_token = std::env::var("AWS_SESSION_TOKEN").ok().filter(|v| !v.is_empty());

    let region = region_from_arn(topic_arn)
        .or_else(|| (!region_conf.is_empty()).then(|| region_conf.to_string()))
        .or_else(|| std::env::var("AWS_REGION").ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "us-east-1".to_string());

    let host = format!("sns.{}.amazonaws.com", region);
    let url = format!("https://{}/", host);

    // Form-encoded Publish body
    let body = format!(
        "Action=Publish&Version=2010-03-31&TopicArn={}&Subject={}&Message={}",
        urlencoding::encode(topic_arn),
        urlencoding::encode(title),
        urlencoding::encode(message),
    );

    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    let payload_hash = sha256_hex(body.as_bytes());

    // Canonical request
    let mut canonical_headers = format!(
        "content-type:application/x-www-form-urlencoded\nhost:{}\nx-amz-date:{}\n",
        host, amz_date,
    );
    let mut signed_headers = String::from("content-type;host;x-amz-date");
    if let Some(ref tok) = session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{}\n", tok));
        signed_headers.push_str(";x-amz-security-token");
    }

    let canonical_request = format!(
        "POST\n/\n\n{}\n{}\n{}",
        canonical_headers, signed_headers, payload_hash,
    );

    // String to sign
    let credential_scope = format!("{}/{}/sns/aws4_request", date_stamp, region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date,
        credential_scope,
        sha256_hex(canonical_request.as_bytes()),
    );

    // Signing key
    let k_date = hmac_sha256(format!("AWS4{}", secret_key).as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"sns");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        access_key, credential_scope, signed_headers, signature,
    );

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Host", &host)
        .header("X-Amz-Date", &amz_date)
        .header("Authorization", &authorization)
        .body(body);
    if let Some(tok) = session_token {
        req = req.header("X-Amz-Security-Token", tok);
    }

    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("SNS publish failed: HTTP {} — {}", status, text);
    }
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    hex::encode(digest.as_ref())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&k, data).as_ref().to_vec()
}

/// Parse region from an ARN: arn:aws:sns:<region>:<account>:<topic>
fn region_from_arn(arn: &str) -> Option<String> {
    let parts: Vec<&str> = arn.splitn(6, ':').collect();
    if parts.len() >= 4 && !parts[3].is_empty() {
        Some(parts[3].to_string())
    } else {
        None
    }
}
