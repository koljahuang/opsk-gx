use axum::{middleware::Next, response::Response};

/// Security headers middleware.
/// Reference: KBP security response headers + OWASP recommendations
pub async fn security_headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    headers.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    headers.insert("X-Frame-Options", "DENY".parse().unwrap());
    headers.insert("X-XSS-Protection", "1; mode=block".parse().unwrap());
    headers.insert("Referrer-Policy", "strict-origin-when-cross-origin".parse().unwrap());
    headers.insert(
        "Strict-Transport-Security",
        "max-age=31536000; includeSubDomains".parse().unwrap(),
    );
    headers.insert(
        "Permissions-Policy",
        "camera=(), microphone=(), geolocation=()".parse().unwrap(),
    );
    // Note: CSP 'default-src self' removed — it blocks cross-origin API calls
    // from the frontend (opsk.kolya.fun → api.opsk.kolya.fun). CSP is meant
    // for HTML pages, not JSON API responses.

    response
}
