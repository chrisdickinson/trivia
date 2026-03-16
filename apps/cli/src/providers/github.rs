use anyhow::{Result, anyhow};
use serde::Deserialize;

use super::{ProviderToken, ProviderUser};

pub struct GitHubProvider {
    client_id: String,
    client_secret: String,
}

#[derive(Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct GitHubUserResponse {
    id: u64,
    login: String,
}

impl GitHubProvider {
    pub fn new(client_id: String, client_secret: String) -> Self {
        Self {
            client_id,
            client_secret,
        }
    }

    pub fn authorize_url(&self, state: &str, redirect_uri: &str) -> String {
        format!(
            "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={}&scope=read:user",
            urlencoding(&self.client_id),
            urlencoding(redirect_uri),
            urlencoding(state),
        )
    }

    pub async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<ProviderToken> {
        let client = reqwest::Client::new();
        let resp = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "client_id": self.client_id,
                "client_secret": self.client_secret,
                "code": code,
                "redirect_uri": redirect_uri,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GitHub token exchange failed: {text}"));
        }

        let token_resp: GitHubTokenResponse = resp.json().await?;
        Ok(ProviderToken {
            access_token: token_resp.access_token,
        })
    }

    pub async fn get_user_info(&self, token: &ProviderToken) -> Result<ProviderUser> {
        let client = reqwest::Client::new();
        let resp = client
            .get("https://api.github.com/user")
            .header("Authorization", format!("Bearer {}", token.access_token))
            .header("User-Agent", "trivia-mcp")
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GitHub user info failed: {text}"));
        }

        let user: GitHubUserResponse = resp.json().await?;
        Ok(ProviderUser {
            provider_user_id: user.id.to_string(),
            username: user.login,
        })
    }
}

fn urlencoding(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}
