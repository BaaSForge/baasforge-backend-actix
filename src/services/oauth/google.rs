use crate::configs::Settings;
use serde::Deserialize;
use thiserror::Error;

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v3/userinfo";
const GOOGLE_SCOPES: &str = "openid email profile";

#[derive(Debug, Error)]
pub enum GoogleOAuthError {
    #[error("Google OAuth is not configured")]
    NotConfigured,
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("token exchange failed: {0}")]
    TokenExchange(String),
    #[error("userinfo request failed: {0}")]
    UserInfo(String),
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct GoogleUserInfo {
    pub sub: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

pub struct GoogleOAuth<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    redirect_uri: &'a str,
    http: reqwest::Client,
}

impl<'a> GoogleOAuth<'a> {
    pub fn from_settings(settings: &'a Settings) -> Result<Self, GoogleOAuthError> {
        let client_id = settings
            .google_oauth_client_id
            .as_deref()
            .ok_or(GoogleOAuthError::NotConfigured)?;
        let client_secret = settings
            .google_oauth_client_secret
            .as_deref()
            .ok_or(GoogleOAuthError::NotConfigured)?;
        let redirect_uri = settings
            .google_oauth_redirect_uri
            .as_deref()
            .ok_or(GoogleOAuthError::NotConfigured)?;

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
            ("response_type", "code"),
            ("scope", GOOGLE_SCOPES),
            ("state", state),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
            ("access_type", "online"),
            ("prompt", "select_account"),
        ];
        let qs: String = query
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{GOOGLE_AUTH_URL}?{qs}")
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
    ) -> Result<String, GoogleOAuthError> {
        let response = self
            .http
            .post(GOOGLE_TOKEN_URL)
            .form(&[
                ("client_id", self.client_id),
                ("client_secret", self.client_secret),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", self.redirect_uri),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GoogleOAuthError::TokenExchange(body));
        }

        let token: GoogleTokenResponse = response.json().await?;
        Ok(token.access_token)
    }

    pub async fn fetch_user_info(
        &self,
        access_token: &str,
    ) -> Result<GoogleUserInfo, GoogleOAuthError> {
        let response = self
            .http
            .get(GOOGLE_USERINFO_URL)
            .bearer_auth(access_token)
            .send()
            .await?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GoogleOAuthError::UserInfo(body));
        }

        response.json().await.map_err(GoogleOAuthError::Http)
    }
}
