//! The app end to end, through its real router and a real database file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::{Router, http::Method, http::StatusCode};
use {{crate_name}}::{AppState, Config, MIGRATOR, routes::router};
use web_mev_test::{Client, TestDb, contract};

const SETUP_TOKEN: &str = "test-setup-token";
const EMAIL: &str = "ann@example.com";
const PASSWORD: &str = "correct horse battery";

struct TestApp {
    db: TestDb,
    state: AppState,
    router: Router,
}

async fn app_with(adjust: impl FnOnce(&mut Config)) -> TestApp {
    let db = TestDb::new(&MIGRATOR).await.unwrap();
    let mut config = Config {
        setup_token: Some(SETUP_TOKEN.to_owned()),
        ..Config::default()
    };
    adjust(&mut config);
    let state = AppState::with_pool(config, db.pool.clone()).unwrap();
    TestApp {
        router: router(state.clone()),
        db,
        state,
    }
}

async fn app() -> TestApp {
    app_with(|_| {}).await
}

impl TestApp {
    fn client(&self) -> Client {
        Client::new(self.router.clone())
    }

    /// Creates the first account, and returns its signed-in browser.
    async fn owner(&self) -> Client {
        let mut client = self.client();
        client.get("/bootstrap").await;
        let response = client
            .post_form(
                "/bootstrap",
                &[
                    ("setup_token", SETUP_TOKEN),
                    ("email", EMAIL),
                    ("display_name", "Ann"),
                    ("password", PASSWORD),
                ],
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        client.get("/").await;
        client
    }

    async fn login(&self, password: &str) -> Client {
        let mut client = self.client();
        client.get("/login").await;
        let response = client
            .post_form("/login", &[("email", EMAIL), ("password", password)])
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        client.get("/").await;
        client
    }
}

#[tokio::test]
async fn the_schema_passes_the_auth_check() {
    let app = app().await;
    web_mev_auth::verify_schema(&app.db.pool).await.unwrap();
}

#[tokio::test]
async fn a_new_instance_leads_to_bootstrap() {
    let app = app().await;
    let mut client = app.client();
    let home = client.get("/").await;
    assert_eq!(home.status, StatusCode::SEE_OTHER);
    assert_eq!(home.location(), Some("/login"));
    let login = client.get("/login").await;
    assert_eq!(login.location(), Some("/bootstrap"));
    let page = client.get("/bootstrap").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("setup_token"));
}

#[tokio::test]
async fn bootstrap_needs_the_token_and_csrf_and_then_closes() {
    let app = app().await;
    let mut client = app.client();
    let fields = |token| {
        [
            ("setup_token", token),
            ("email", EMAIL),
            ("display_name", "Ann"),
            ("password", PASSWORD),
        ]
    };
    let forged = client
        .post_raw_form("/bootstrap", &fields(SETUP_TOKEN))
        .await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN, "no CSRF token");

    client.get("/bootstrap").await;
    let wrong = client.post_form("/bootstrap", &fields("wrong")).await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    assert!(
        wrong.body.contains("setup token is wrong"),
        "{}",
        wrong.body
    );

    let right = client.post_form("/bootstrap", &fields(SETUP_TOKEN)).await;
    assert_eq!(right.status, StatusCode::SEE_OTHER);
    assert_eq!(client.get("/").await.status, StatusCode::OK);

    let mut other = app.client();
    assert_eq!(other.get("/bootstrap").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn short_passwords_are_refused_with_the_form_kept() {
    let app = app().await;
    let mut client = app.client();
    client.get("/bootstrap").await;
    let response = client
        .post_form(
            "/bootstrap",
            &[
                ("setup_token", SETUP_TOKEN),
                ("email", EMAIL),
                ("display_name", "Ann"),
                ("password", "short"),
            ],
        )
        .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert!(response.body.contains("12 to 1024"), "{}", response.body);
    assert!(response.body.contains(EMAIL));
}

#[tokio::test]
async fn login_works_and_failures_lock_the_account() {
    let app = app().await;
    drop(app.owner().await);
    let mut client = app.client();
    client.get("/login").await;
    for _ in 0..5 {
        let response = client
            .post_form(
                "/login",
                &[("email", EMAIL), ("password", "wrong password")],
            )
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED);
        assert!(response.body.contains("Wrong email or password"));
    }
    let locked = client
        .post_form("/login", &[("email", EMAIL), ("password", PASSWORD)])
        .await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.body.contains("Too many failed attempts"));
}

#[tokio::test]
async fn a_post_without_csrf_is_refused() {
    let app = app().await;
    let mut client = app.owner().await;
    let forged = client.post_raw_form("/notes", &[("body", "x")]).await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    let notes: i64 = sqlx::query_scalar("SELECT count(*) FROM notes")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(notes, 0);
}

#[tokio::test]
async fn notes_flash_once_without_htmx() {
    let app = app().await;
    let mut client = app.owner().await;
    let response = client.post_form("/notes", &[("body", "first note")]).await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    let page = client.get("/").await;
    assert!(page.body.contains("first note"));
    assert!(page.body.contains("Note added."));
    let page = client.get("/").await;
    assert!(page.body.contains("first note"));
    assert!(!page.body.contains("Note added."), "a flash shows once");
}

#[tokio::test]
async fn notes_swap_a_fragment_with_htmx() {
    let app = app().await;
    let mut client = app.owner().await;
    let response = client.htmx_post("/notes", &[("body", "<b>note</b>")]).await;
    assert_eq!(response.status, StatusCode::OK);
    assert!(!response.body.contains("<html"), "only the fragment");
    assert!(
        response.body.contains("&lt;b&gt;note&lt;/b&gt;"),
        "{}",
        response.body
    );
    assert!(
        response
            .header("hx-trigger")
            .unwrap()
            .contains("Note added.")
    );
}

#[tokio::test]
async fn logout_needs_post_and_csrf() {
    let app = app().await;
    let mut client = app.owner().await;
    assert_eq!(
        client.get("/logout").await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        client.post_raw_form("/logout", &[]).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(client.get("/").await.status, StatusCode::OK);
    assert_eq!(
        client.post_form("/logout", &[]).await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(client.get("/").await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn a_password_change_signs_everyone_out() {
    let app = app().await;
    let mut first = app.owner().await;
    let mut second = app.login(PASSWORD).await;
    second.get("/settings").await;

    let wrong = second
        .post_form(
            "/settings/password",
            &[
                ("current_password", "wrong password"),
                ("new_password", "a new long password"),
            ],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(first.get("/").await.status, StatusCode::OK);

    let changed = second
        .post_form(
            "/settings/password",
            &[
                ("current_password", PASSWORD),
                ("new_password", "a new long password"),
            ],
        )
        .await;
    assert_eq!(changed.status, StatusCode::SEE_OTHER);
    assert_eq!(changed.location(), Some("/login"));

    assert_eq!(
        first.get("/").await.status,
        StatusCode::SEE_OTHER,
        "other sessions end"
    );
    assert_eq!(second.get("/").await.status, StatusCode::SEE_OTHER);
    let login_page = second.get("/login").await;
    assert!(
        login_page.body.contains("Password changed."),
        "{}",
        login_page.body
    );

    let mut stale = app.client();
    stale.get("/login").await;
    let old = stale
        .post_form("/login", &[("email", EMAIL), ("password", PASSWORD)])
        .await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    drop(app.login("a new long password").await);

    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'user.password_changed'")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(audits, 1);
}

#[tokio::test]
async fn pages_render_in_every_language() {
    let app = app().await;
    let mut client = app.owner().await;
    for (code, title, count) in [("en", "Notes", "1 note"), ("pl", "Notatki", "1 notatka")] {
        client.post_form("/notes", &[("body", "x")]).await;
        sqlx::query("DELETE FROM notes WHERE id NOT IN (SELECT max(id) FROM notes)")
            .execute(&app.db.pool)
            .await
            .unwrap();
        let switched = client.get(&format!("/locale/{code}?next=/settings")).await;
        assert_eq!(switched.location(), Some("/settings"));
        let home = client.get("/").await;
        assert_eq!(home.status, StatusCode::OK);
        assert!(
            home.body.contains(&format!(r#"<html lang="{code}">"#)),
            "{}",
            home.body
        );
        assert!(home.body.contains(title), "{code}: {}", home.body);
        assert!(home.body.contains(count), "{code}: {}", home.body);
        assert_eq!(client.get("/settings").await.status, StatusCode::OK);
    }
    assert_eq!(client.get("/locale/de").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn every_language_has_every_string() {
    let app = app().await;
    let catalog = app.state.i18n.catalog();
    let plural = |key: &str| {
        ["_one", "_few", "_many", "_other"]
            .iter()
            .any(|end| key.ends_with(end))
    };
    for key in catalog.keys().filter(|key| !plural(key)) {
        for locale in catalog.locales() {
            assert!(
                catalog.tr(key, locale).is_some(),
                "{key} is missing in {locale}"
            );
        }
    }
}

#[tokio::test]
async fn accept_language_picks_a_first_language() {
    let app = app().await;
    let mut client = app.client();
    client.get("/login").await;
    let page = client
        .request(
            Method::GET,
            "/bootstrap",
            &[("accept-language", "pl-PL,pl;q=0.9")],
            None,
        )
        .await;
    assert!(page.body.contains(r#"<html lang="pl">"#), "{}", page.body);
}

#[tokio::test]
async fn language_switch_stays_on_this_site() {
    let app = app().await;
    let mut client = app.client();
    let response = client.get("/locale/pl?next=//evil.example").await;
    assert_eq!(response.location(), Some("/"));
}

#[tokio::test]
async fn security_headers_cover_pages_and_the_fallback() {
    let app = app().await;
    let mut client = app.client();
    for path in ["/bootstrap", "/nope", "/api/v1/nope", "/health"] {
        let response = client.get(path).await;
        let csp = response.header("content-security-policy").unwrap();
        assert!(csp.starts_with("default-src 'self'"), "{path}: {csp}");
        assert_eq!(
            response.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
    }
    let page = client.get("/bootstrap").await;
    assert!(
        !page.body.contains(" style=\""),
        "the CSP forbids inline styles"
    );
    assert!(
        !page.body.contains("onclick"),
        "the CSP forbids inline handlers"
    );
    assert!(
        !page.body.contains("<script>"),
        "the CSP forbids inline scripts"
    );
}

#[tokio::test]
async fn error_pages_escape_and_avoid_tera() {
    let app = app().await;
    let mut client = app.client();
    let response = client.get("/nope").await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert!(response.body.contains("Not Found"), "{}", response.body);
    assert!(response.body.contains("/static/app.css"));
}

#[tokio::test]
async fn the_api_answers_with_json() {
    let app = app().await;
    let mut client = app.client();
    contract::unknown_route_is_json_404(&mut client, "/api/v1/nope").await;
    contract::wrong_method_is_json_405(&mut client, Method::DELETE, "/api/v1/me").await;
    let anonymous = client.get("/api/v1/me").await;
    contract::assert_json_error(&anonymous, StatusCode::UNAUTHORIZED, "unauthorized");

    let mut owner = app.owner().await;
    let me = owner.get("/api/v1/me").await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json()["email"], EMAIL);
    assert!(me.json().get("password_hash").is_none());
}

#[tokio::test]
async fn health_and_readiness_answer_anonymously() {
    let app = app().await;
    let mut client = app.client();
    assert_eq!(client.get("/health").await.status, StatusCode::OK);
    assert_eq!(client.get("/ready").await.status, StatusCode::OK);
}

#[tokio::test]
async fn static_files_are_versioned_compressed_and_safe() {
    let app = app().await;
    let mut client = app.client();
    let url = app.state.assets.url("app.css");
    assert!(url.contains("?v="), "{url}");
    let response = client
        .request(Method::GET, &url, &[("accept-encoding", "br")], None)
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-encoding"), Some("br"));
    assert!(
        response
            .header("cache-control")
            .unwrap()
            .contains("immutable")
    );
    assert_eq!(response.header("vary"), Some("accept-encoding"));

    let stale = client.get("/static/app.css?v=stale").await;
    assert!(!stale.header("cache-control").unwrap().contains("immutable"));
    assert_eq!(
        client.get("/static/nope.css").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client.get("/static/../Cargo.toml").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn every_static_url_in_a_page_resolves() {
    let app = app().await;
    let mut client = app.client();
    let page = client.get("/bootstrap").await;
    let mut found = 0;
    for piece in page
        .body
        .split('"')
        .filter(|piece| piece.starts_with("/static/"))
    {
        found += 1;
        assert_eq!(client.get(piece).await.status, StatusCode::OK, "{piece}");
    }
    assert!(found >= 3, "{}", page.body);
}

#[tokio::test]
async fn rate_limits_apply_but_not_to_health() {
    let app = app_with(|config| config.rate_limit_per_source = 2).await;
    let mut client = app.client();
    client.get("/login").await;
    client.get("/login").await;
    let refused = client.get("/login").await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(client.get("/health").await.status, StatusCode::OK);
}
