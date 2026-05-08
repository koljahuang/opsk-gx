//! Sync pipeline_repos to ArgoCD repository Secrets.
//!
//! When a user creates/updates/deletes a Code Repo in Ops K,
//! we mirror the credentials as a Secret in the `argocd` namespace
//! so ArgoCD can access the repo for GitOps sync.

use k8s_openapi::api::core::v1::Secret;
use kube::api::{DeleteParams, Patch, PatchParams};
use kube::{Api, Client};
use std::collections::BTreeMap;
use uuid::Uuid;

const ARGOCD_NAMESPACE: &str = "argocd";

/// Build an in-cluster kube client (backend pod runs in the same cluster as ArgoCD).
async fn local_client() -> Result<Client, kube::Error> {
    Client::try_default().await
}

/// Deterministic Secret name from repo UUID.
fn secret_name(repo_id: Uuid) -> String {
    format!("opsk-repo-{}", repo_id)
}

/// Create or update an ArgoCD repository Secret so ArgoCD can access the git repo.
/// Call this after create/update of a pipeline_repo when enabled=true and token is available.
pub async fn sync_repo_secret(repo_id: Uuid, repo_url: &str, token: &str) -> Result<(), String> {
    let client = local_client().await.map_err(|e| e.to_string())?;
    let secrets: Api<Secret> = Api::namespaced(client, ARGOCD_NAMESPACE);
    let name = secret_name(repo_id);

    let mut labels = BTreeMap::new();
    labels.insert("argocd.argoproj.io/secret-type".to_string(), "repository".to_string());
    labels.insert("app.kubernetes.io/managed-by".to_string(), "opsk".to_string());

    let mut string_data = BTreeMap::new();
    string_data.insert("type".to_string(), "git".to_string());
    string_data.insert("url".to_string(), repo_url.to_string());
    string_data.insert("username".to_string(), "x-access-token".to_string());
    string_data.insert("password".to_string(), token.to_string());

    let secret = Secret {
        metadata: kube::api::ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(ARGOCD_NAMESPACE.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        string_data: Some(string_data),
        ..Default::default()
    };

    secrets
        .patch(&name, &PatchParams::apply("opsk"), &Patch::Apply(secret))
        .await
        .map_err(|e| e.to_string())?;

    tracing::info!("ArgoCD repo secret synced: {}", name);
    Ok(())
}

/// Delete the ArgoCD repository Secret. Ignores 404 (already gone).
pub async fn delete_repo_secret(repo_id: Uuid) -> Result<(), String> {
    let client = local_client().await.map_err(|e| e.to_string())?;
    let secrets: Api<Secret> = Api::namespaced(client, ARGOCD_NAMESPACE);
    let name = secret_name(repo_id);

    match secrets.delete(&name, &DeleteParams::default()).await {
        Ok(_) => {
            tracing::info!("ArgoCD repo secret deleted: {}", name);
            Ok(())
        }
        Err(kube::Error::Api(resp)) if resp.code == 404 => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
