use crate::config::CognitoOAuthConfig;
use crate::error::{AppError, AppResult};

/// Extract a human-readable message from an AWS SDK error.
fn describe_sdk_error<E: std::fmt::Debug + std::fmt::Display, R: std::fmt::Debug>(
    err: &aws_sdk_cognitoidentityprovider::error::SdkError<E, R>,
) -> String {
    match err {
        aws_sdk_cognitoidentityprovider::error::SdkError::ServiceError(service_err) => {
            format!("{}", service_err.err())
        }
        other => format!("{other}"),
    }
}

async fn build_client(config: &CognitoOAuthConfig) -> aws_sdk_cognitoidentityprovider::Client {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(config.region.clone()))
        .load()
        .await;
    aws_sdk_cognitoidentityprovider::Client::new(&aws_config)
}

/// Create a user in Cognito user pool with a temporary password.
/// Uses the email prefix as Cognito username (alias_attributes pools reject email-format usernames).
/// Returns the Cognito `sub` (user ID).
pub async fn create_cognito_user(config: &CognitoOAuthConfig, email: &str, temp_password: &str) -> AppResult<String> {
    let client = build_client(config).await;
    let cognito_username = email.split('@').next().unwrap_or(email).to_string();

    let create_result = client
        .admin_create_user()
        .user_pool_id(&config.user_pool_id)
        .username(&cognito_username)
        .temporary_password(temp_password)
        .message_action(aws_sdk_cognitoidentityprovider::types::MessageActionType::Suppress)
        .user_attributes(
            aws_sdk_cognitoidentityprovider::types::AttributeType::builder()
                .name("email")
                .value(email)
                .build()
                .map_err(|e| AppError::Internal(format!("Failed to build attribute: {e}")))?,
        )
        .user_attributes(
            aws_sdk_cognitoidentityprovider::types::AttributeType::builder()
                .name("email_verified")
                .value("true")
                .build()
                .map_err(|e| AppError::Internal(format!("Failed to build attribute: {e}")))?,
        )
        .send()
        .await
        .map_err(|e| {
            let detail = describe_sdk_error(&e);
            tracing::error!("Cognito AdminCreateUser failed for {}: {}", email, detail);
            if detail.contains("UsernameExistsException") || detail.contains("already exists") {
                AppError::Conflict(format!("User with email '{email}' already exists in Cognito"))
            } else if detail.contains("AccessDeniedException") || detail.contains("not authorized") {
                AppError::Internal(format!("Cognito permission denied: {detail}"))
            } else {
                AppError::Internal(format!("Cognito AdminCreateUser failed: {detail}"))
            }
        })?;

    let sub = create_result
        .user()
        .and_then(|u| {
            u.attributes().iter().find_map(|attr| {
                if attr.name() == "sub" {
                    attr.value().map(|v| v.to_string())
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| AppError::Internal("Cognito user created but sub not returned".to_string()))?;

    // Set the password as permanent — use cognito_username (not sub, which admin APIs don't accept)
    client
        .admin_set_user_password()
        .user_pool_id(&config.user_pool_id)
        .username(&cognito_username)
        .password(temp_password)
        .permanent(true)
        .send()
        .await
        .map_err(|e| {
            let detail = describe_sdk_error(&e);
            tracing::error!("Cognito AdminSetUserPassword failed for {}: {}", email, detail);
            AppError::Internal(format!("Cognito AdminSetUserPassword failed: {detail}"))
        })?;

    Ok(sub)
}

/// Set a user's password in Cognito (used after invite redeem or password change).
/// Uses email as identifier (email is an alias attribute, accepted by admin APIs).
pub async fn set_cognito_password(config: &CognitoOAuthConfig, email: &str, password: &str) -> AppResult<()> {
    let client = build_client(config).await;

    client
        .admin_set_user_password()
        .user_pool_id(&config.user_pool_id)
        .username(email)
        .password(password)
        .permanent(true)
        .send()
        .await
        .map_err(|e| {
            let detail = describe_sdk_error(&e);
            tracing::error!("Cognito set password failed for {}: {}", email, detail);
            AppError::Internal(format!("Cognito set password failed: {detail}"))
        })?;

    Ok(())
}

/// Delete a user from Cognito (best-effort, logs errors).
pub async fn delete_cognito_user(config: &CognitoOAuthConfig, email: &str) -> AppResult<()> {
    let client = build_client(config).await;

    if let Err(e) = client
        .admin_delete_user()
        .user_pool_id(&config.user_pool_id)
        .username(email)
        .send()
        .await
    {
        tracing::warn!("Failed to delete Cognito user {}: {}", email, e);
    }

    Ok(())
}
