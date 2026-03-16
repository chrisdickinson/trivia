pub mod github;

use anyhow::Result;

pub struct ProviderUser {
    pub provider_user_id: String,
    pub username: String,
}

pub struct ProviderToken {
    pub access_token: String,
}

/// Enum dispatch over supported OAuth providers.
pub enum Provider {
    GitHub(github::GitHubProvider),
}

impl Provider {
    /// Build from a DB provider record.
    pub fn from_db(db_provider: &trivia_core::OAuthProvider) -> Result<Self> {
        match db_provider.provider_type.as_str() {
            "github" => Ok(Provider::GitHub(github::GitHubProvider::new(
                db_provider.client_id.clone(),
                db_provider.client_secret.clone(),
            ))),
            other => anyhow::bail!("unsupported provider type: {other}"),
        }
    }

    pub fn authorize_url(&self, state: &str, redirect_uri: &str) -> String {
        match self {
            Provider::GitHub(p) => p.authorize_url(state, redirect_uri),
        }
    }

    pub async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<ProviderToken> {
        match self {
            Provider::GitHub(p) => p.exchange_code(code, redirect_uri).await,
        }
    }

    pub async fn get_user_info(&self, token: &ProviderToken) -> Result<ProviderUser> {
        match self {
            Provider::GitHub(p) => p.get_user_info(token).await,
        }
    }
}
