use crate::configs::Settings;
use serde::Deserialize;
use thiserror::Error;

const GITHUB_AUTH_URL: &str = "https://github.com/login/oauth/authorize";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_USER_URL: &str = "https://api.github.com/user";
const GITHUB_USER_EMAILS_URL: &str = "https://api.github.com/user/emails";
const GITHUB_SCOPES: &str = "read:user user:email";

#[derive(Debug, Error)]
pub enum GitHubOAuthError {
    #[error("GitHub OAuth is not configured")]
    NotConfigured,
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("token exchange failed: {0}")]
    TokenExchange(String),
    #[error("user request failed: {0}")]
    UserInfo(String),
}

#[derive(Debug, Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct GitHubUserResponse {
    id: i64,
    name: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubEmailEntry {
    email: String,
    primary: bool,
    verified: bool,
}

#[derive(Debug)]
pub struct GitHubUserInfo {
    pub id: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

pub struct GitHubOAuth<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    redirect_uri: &'a str,
    http: reqwest::Client,
}

impl<'a> GitHubOAuth<'a> {
    pub fn from_settings(settings: &'a Settings) -> Result<Self, GitHubOAuthError> {
        let client_id = settings
            .github_oauth_client_id
            .as_deref()
            .ok_or(GitHubOAuthError::NotConfigured)?;
        let client_secret = settings
            .github_oauth_client_secret
            .as_deref()
            .ok_or(GitHubOAuthError::NotConfigured)?;
        let redirect_uri = settings
            .github_oauth_redirect_uri
            .as_deref()
            .ok_or(GitHubOAuthError::NotConfigured)?;

        Ok(Self {
            client_id,
            client_secret,
            redirect_uri,
            http: reqwest::Client::new(),
        })
    }

    pub fn authorization_url(&self, state: &str, code_challenge: &str) -> String {
        let query = [
            ("client_id", self.client_id),
            ("redirect_uri", self.redirect_uri),
            ("scope", GITHUB_SCOPES),
            ("state", state),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
        ];
        let qs: String = query
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{GITHUB_AUTH_URL}?{qs}")
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
    ) -> Result<String, GitHubOAuthError> {
        let response = self
            .http
            .post(GITHUB_TOKEN_URL)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", self.client_id),
                ("client_secret", self.client_secret),
                ("code", code),
                ("redirect_uri", self.redirect_uri),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GitHubOAuthError::TokenExchange(body));
        }

        let token: GitHubTokenResponse = response.json().await?;
        Ok(token.access_token)
    }

    pub async fn fetch_user_info(
        &self,
        access_token: &str,
    ) -> Result<GitHubUserInfo, GitHubOAuthError> {
        let response = self
            .http
            .get(GITHUB_USER_URL)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "baal-backend")
            .bearer_auth(access_token)
            .send()
            .await?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GitHubOAuthError::UserInfo(body));
        }

        let user: GitHubUserResponse = response.json().await.map_err(GitHubOAuthError::Http)?;
        let email = match user.email {
            Some(email) => Some(email),
            None => self.fetch_primary_email(access_token).await?,
        };

        Ok(GitHubUserInfo {
            id: user.id.to_string(),
            name: user.name,
            email,
        })
    }

    async fn fetch_primary_email(
        &self,
        access_token: &str,
    ) -> Result<Option<String>, GitHubOAuthError> {
        let response = self
            .http
            .get(GITHUB_USER_EMAILS_URL)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "baal-backend")
            .bearer_auth(access_token)
            .send()
            .await?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GitHubOAuthError::UserInfo(body));
        }

        let emails: Vec<GitHubEmailEntry> =
            response.json().await.map_err(GitHubOAuthError::Http)?;

        Ok(emails
            .iter()
            .find(|e| e.primary && e.verified)
            .or_else(|| emails.iter().find(|e| e.primary))
            .or_else(|| emails.first())
            .map(|e| e.email.clone()))
    }
}
