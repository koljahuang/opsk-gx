mod helpers;

use opsk::error::AppError;
use opsk::middleware::auth::AuthUser;
use opsk::models::provider::{CreateProviderRequest, UpdateProviderRequest};
use opsk::services::provider;
use sqlx::PgPool;
use uuid::Uuid;

fn super_admin() -> AuthUser {
    AuthUser {
        user_id: Uuid::new_v4(),
        role: "super_admin".to_string(),
        tenant_id: None,
        username: "admin".to_string(),
    }
}

fn admin_with_tenant(tenant_id: Uuid) -> AuthUser {
    AuthUser {
        user_id: Uuid::new_v4(),
        role: "tenant_admin".to_string(),
        tenant_id: Some(tenant_id),
        username: "ta".to_string(),
    }
}

fn member(tenant_id: Uuid) -> AuthUser {
    AuthUser {
        user_id: Uuid::new_v4(),
        role: "member".to_string(),
        tenant_id: Some(tenant_id),
        username: "m".to_string(),
    }
}

async fn seed_tenant(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("INSERT INTO tenants (name, slug) VALUES ('t', 't') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn make_req(name: &str) -> CreateProviderRequest {
    CreateProviderRequest {
        name: name.to_string(),
        provider_type: "gateway".to_string(),
        config: serde_json::json!({}),
    }
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_list_empty(pool: PgPool) {
    let admin = super_admin();
    let result = provider::list(&pool, &admin).await.unwrap();
    assert!(result.is_empty());
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_create_super_admin_only(pool: PgPool) {
    let admin = super_admin();
    let p = provider::create(&pool, &admin, make_req("First")).await.unwrap();
    assert_eq!(p.name, "First");
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_create_empty_name_rejected(pool: PgPool) {
    let admin = super_admin();

    let result = provider::create(&pool, &admin, make_req("")).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        AppError::BadRequest(msg) => assert!(msg.contains("Name"), "Expected 'Name' in message, got: {}", msg),
        other => panic!("Expected BadRequest, got {:?}", other),
    }
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_create_non_super_admin_forbidden(pool: PgPool) {
    let tid = seed_tenant(&pool).await;
    let ta = admin_with_tenant(tid);

    let result = provider::create(&pool, &ta, make_req("Nope")).await;
    assert!(matches!(result, Err(AppError::Forbidden(_))));

    let m = member(tid);
    let result = provider::create(&pool, &m, make_req("Nope")).await;
    assert!(matches!(result, Err(AppError::Forbidden(_))));
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_assign_and_list_by_tenant(pool: PgPool) {
    let admin = super_admin();
    let tid = seed_tenant(&pool).await;

    let p1 = provider::create(&pool, &admin, make_req("Model A")).await.unwrap();
    let p2 = provider::create(&pool, &admin, make_req("Model B")).await.unwrap();

    // Assign both to tenant
    provider::assign_to_tenant(&pool, &admin, tid, vec![p1.id, p2.id])
        .await
        .unwrap();

    let assigned = provider::list_by_tenant(&pool, &admin, tid).await.unwrap();
    assert_eq!(assigned.len(), 2);

    // First assigned gets auto-default
    let default_count = assigned.iter().filter(|a| a.is_default).count();
    assert_eq!(default_count, 1);
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_set_tenant_default(pool: PgPool) {
    let admin = super_admin();
    let tid = seed_tenant(&pool).await;

    let p1 = provider::create(&pool, &admin, make_req("Model A")).await.unwrap();
    let p2 = provider::create(&pool, &admin, make_req("Model B")).await.unwrap();

    provider::assign_to_tenant(&pool, &admin, tid, vec![p1.id, p2.id])
        .await
        .unwrap();

    // Set p2 as default
    provider::set_tenant_default(&pool, &admin, tid, p2.id).await.unwrap();

    let assigned = provider::list_by_tenant(&pool, &admin, tid).await.unwrap();
    for a in &assigned {
        if a.provider.id == p2.id {
            assert!(a.is_default);
        } else {
            assert!(!a.is_default);
        }
    }
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_set_default_unassigned_provider_fails(pool: PgPool) {
    let admin = super_admin();
    let tid = seed_tenant(&pool).await;
    let p = provider::create(&pool, &admin, make_req("Model")).await.unwrap();

    // Don't assign, try to set default
    let result = provider::set_tenant_default(&pool, &admin, tid, p.id).await;
    assert!(matches!(result, Err(AppError::BadRequest(_))));
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_reassign_removes_old(pool: PgPool) {
    let admin = super_admin();
    let tid = seed_tenant(&pool).await;

    let p1 = provider::create(&pool, &admin, make_req("Model A")).await.unwrap();
    let p2 = provider::create(&pool, &admin, make_req("Model B")).await.unwrap();

    // Assign both
    provider::assign_to_tenant(&pool, &admin, tid, vec![p1.id, p2.id])
        .await
        .unwrap();
    assert_eq!(provider::list_by_tenant(&pool, &admin, tid).await.unwrap().len(), 2);

    // Reassign only p2
    provider::assign_to_tenant(&pool, &admin, tid, vec![p2.id])
        .await
        .unwrap();
    let assigned = provider::list_by_tenant(&pool, &admin, tid).await.unwrap();
    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].provider.id, p2.id);
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_tenant_user_sees_only_assigned(pool: PgPool) {
    let admin = super_admin();
    let tid = seed_tenant(&pool).await;

    let p1 = provider::create(&pool, &admin, make_req("Model A")).await.unwrap();
    let _p2 = provider::create(&pool, &admin, make_req("Model B")).await.unwrap();

    // Only assign p1 to tenant
    provider::assign_to_tenant(&pool, &admin, tid, vec![p1.id])
        .await
        .unwrap();

    // Tenant user should only see p1
    let ta = admin_with_tenant(tid);
    let visible = provider::list(&pool, &ta).await.unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].provider.id, p1.id);
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_delete_provider(pool: PgPool) {
    let admin = super_admin();
    let p = provider::create(&pool, &admin, make_req("ToDelete")).await.unwrap();

    provider::delete(&pool, &admin, p.id).await.unwrap();

    let all = provider::list(&pool, &admin).await.unwrap();
    assert!(all.is_empty());
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_update_success(pool: PgPool) {
    let admin = super_admin();

    let p = provider::create(&pool, &admin, make_req("Original")).await.unwrap();
    assert_eq!(p.name, "Original");

    let updated = provider::update(
        &pool,
        &admin,
        p.id,
        UpdateProviderRequest {
            name: Some("Renamed".to_string()),
            provider_type: None,
            config: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(updated.name, "Renamed");
    assert_eq!(updated.id, p.id);
}

#[sqlx::test(migrations = "src/migrations")]
async fn test_update_not_found(pool: PgPool) {
    let admin = super_admin();

    let result = provider::update(
        &pool,
        &admin,
        Uuid::new_v4(),
        UpdateProviderRequest {
            name: Some("Ghost".to_string()),
            provider_type: None,
            config: None,
        },
    )
    .await;

    assert!(matches!(result, Err(AppError::NotFound(_))));
}

#[test]
fn test_available_types_local() {
    let types = provider::available_types(true);
    assert_eq!(types.len(), 2);
    let values: Vec<&str> = types.iter().map(|t| t.value.as_str()).collect();
    assert!(values.contains(&"bedrock"));
    assert!(values.contains(&"gateway"));
}

#[test]
fn test_available_types_non_local() {
    let types = provider::available_types(false);
    assert_eq!(types.len(), 1);
    assert_eq!(types[0].value, "gateway");
}
