//! Reusable checks of what an API promises at its HTTP boundary. Call them from
//! an app's own test suite, against its real router.

use axum::http::{Method, StatusCode};

use crate::client::{Client, TestResponse};

/// Asserts a JSON error with this status and code, and the `{code, message, field}`
/// shape.
pub fn assert_json_error(response: &TestResponse, status: StatusCode, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    let content_type = response.header("content-type").unwrap_or_default();
    assert!(
        content_type.starts_with("application/json"),
        "not JSON ({content_type}): {}",
        response.body
    );
    let body = response.json();
    assert_eq!(body["code"], code, "{}", response.body);
    assert!(body["message"].is_string(), "{}", response.body);
    assert!(body.get("field").is_some(), "{}", response.body);
}

/// An unknown route answers with the JSON 404, not an empty or HTML body.
pub async fn unknown_route_is_json_404(client: &mut Client, path: &str) {
    let response = client.get(path).await;
    assert_json_error(&response, StatusCode::NOT_FOUND, "not_found");
}

/// A known route answers another method with the JSON 405.
pub async fn wrong_method_is_json_405(client: &mut Client, method: Method, path: &str) {
    let response = client.request(method, path, &[], None).await;
    assert_json_error(
        &response,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    );
}

/// A JSON body over `limit` bytes answers with the JSON 413. `headers` should
/// hold whatever credentials the route needs.
pub async fn oversized_body_is_json_413(
    client: &mut Client,
    path: &str,
    headers: &[(&str, &str)],
    limit: usize,
) {
    let mut all = vec![("content-type", "application/json")];
    all.extend_from_slice(headers);
    let response = client
        .request(Method::POST, path, &all, Some("x".repeat(limit + 1)))
        .await;
    assert_json_error(
        &response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    );
}

/// With no credentials, a body that is invalid and oversized still answers 401,
/// so anonymous callers can't make the server parse anything.
pub async fn auth_runs_before_the_body(client: &mut Client, path: &str, limit: usize) {
    let response = client
        .request(
            Method::POST,
            path,
            &[("content-type", "application/json")],
            Some("x".repeat(limit + 1)),
        )
        .await;
    assert_json_error(&response, StatusCode::UNAUTHORIZED, "unauthorized");
    assert_eq!(response.header("www-authenticate"), Some("Bearer"));
}
