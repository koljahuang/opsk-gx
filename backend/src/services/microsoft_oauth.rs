use serde::Deserialize;

use crate::config::MicrosoftOAuthConfig;
use crate::error::{AppError, AppResult};

#[derive(Debug, Deserialize)]
pub struct MicrosoftTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrosoftUserInfo {
    pub id: String,
    pub mail: Option<String>,
    pub user_principal_name: Option<String>,
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub surname: Option<String>,
}

impl MicrosoftUserInfo {
    /// Get the best available email
    pub fn email(&self) -> Option<String> {
        self.mail.clone().or_else(|| self.user_principal_name.clone())
    }

    /// Get display name or fall back to email
    pub fn display_name_or_email(&self) -> String {
        self.display_name
            .clone()
            .or_else(|| match (&self.given_name, &self.surname) {
                (Some(g), Some(s)) => Some(format!("{} {}", g, s)),
                (Some(g), None) => Some(g.clone()),
                _ => None,
            })
            .or_else(|| self.email())
            .unwrap_or_else(|| "Unknown".to_string())
    }
}

/// Build the Microsoft authorization URL
pub fn get_authorization_url(
    config: &MicrosoftOAuthConfig,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize?\
         client_id={}&response_type=code&redirect_uri={}&scope={}&state={}\
         &code_challenge={}&code_challenge_method=S256&response_mode=query",
        config.tenant_id,
        config.client_id,
        urlencoding::encode(redirect_uri),
        urlencoding::encode("openid profile email User.Read GroupMember.Read.All"),
        state,
        code_challenge,
    )
}

/// Exchange authorization code for tokens
pub async fn exchange_code_for_token(
    config: &MicrosoftOAuthConfig,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> AppResult<MicrosoftTokenResponse> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        config.tenant_id
    );

    let response = client
        .post(&url)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| AppError::HttpClient(e.to_string()))?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(AppError::OAuth(format!(
            "Microsoft token exchange failed: {}",
            error_text
        )));
    }

    response
        .json::<MicrosoftTokenResponse>()
        .await
        .map_err(|e| AppError::HttpClient(e.to_string()))
}

// ─── Group membership types ────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GraphMemberOfResponse {
    value: Vec<GraphDirectoryObject>,
}

#[derive(Debug, Deserialize)]
struct GraphDirectoryObject {
    #[serde(rename = "@odata.type")]
    odata_type: Option<String>,
    id: Option<String>,
}

/// Fetch the user's Entra ID group memberships via Graph API.
/// Returns group IDs. Gracefully returns empty vec on failure (scope not granted).
pub async fn get_user_groups(access_token: &str) -> Vec<String> {
    let client = reqwest::Client::new();
    let response = match client
        .get("https://graph.microsoft.com/v1.0/me/memberOf")
        .bearer_auth(access_token)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Failed to fetch user groups from Graph API: {}", e);
            return vec![];
        }
    };

    if !response.status().is_success() {
        tracing::warn!(
            "Graph API /me/memberOf returned {}: group-based mapping will be skipped",
            response.status()
        );
        return vec![];
    }

    match response.json::<GraphMemberOfResponse>().await {
        Ok(data) => data
            .value
            .into_iter()
            .filter(|obj| obj.odata_type.as_deref() == Some("#microsoft.graph.group"))
            .filter_map(|obj| obj.id)
            .collect(),
        Err(e) => {
            tracing::warn!("Failed to parse Graph API memberOf response: {}", e);
            vec![]
        }
    }
}

/// Get user info from Microsoft Graph API
pub async fn get_user_info(access_token: &str) -> AppResult<MicrosoftUserInfo> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://graph.microsoft.com/v1.0/me")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| AppError::HttpClient(e.to_string()))?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(AppError::OAuth(format!("Microsoft user info failed: {}", error_text)));
    }

    response
        .json::<MicrosoftUserInfo>()
        .await
        .map_err(|e| AppError::HttpClient(e.to_string()))
}
