use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{delete, post},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Default)]
struct Fixture {
    initialized: Notify,
    release_init: Notify,
    mcp_sends: AtomicUsize,
    deleted: AtomicUsize,
}

async fn fixture() -> (String, Arc<Fixture>) {
    let f = Arc::new(Fixture::default());
    let router = Router::new()
        .route("/mcp", post(|State(f): State<Arc<Fixture>>, Json(req): Json<Value>| async move {
            f.mcp_sends.fetch_add(1, Ordering::SeqCst);
            if req["method"] == "initialize" {
                f.initialized.notify_one();
                f.release_init.notified().await;
                Json(json!({"jsonrpc": "2.0", "id": req["id"], "result": {"protocolVersion": "2025-11-25"}}))
            } else {
                Json(json!({"jsonrpc": "2.0", "id": req["id"], "result": {"structuredContent": {"results": []}}}))
            }
        }))
        .route("/search", post(|| async { Json(json!({"results": []})) }))
        .route("/v1/web-devices/wd_fixture", delete(|State(f): State<Arc<Fixture>>| async move {
            f.deleted.fetch_add(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }))
        .with_state(f.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, f)
}

#[tokio::test]
async fn desktop_provider_test_binding_gates_mcp_sends_and_state_publication() {
    let (base, f) = fixture().await;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    // A bound session without background workers, so only explicit tests
    // contact the fixture.
    let web = WebSearch::new(root, false, Duration::from_millis(25), Events::new(root))
        .with_endpoints(Endpoints {
            exa: format!("{base}/mcp"),
            tavily: base.clone(),
            ..Endpoints::vendor()
        });
    let session = Secret::new("fixture-session");
    web.session_bound(&base, "alice", &session, 1).await;
    let profile = web.profile(&base, "alice").unwrap();
    let engine = web.engine(&profile).unwrap();
    let lease = web.test_lease(&base, "alice", 1).await.unwrap();

    // Have a result ready to save before the session ends. It must not
    // update status or clear an existing cooldown once the binding changes.
    let cooldown = settings::Updates {
        cooldowns: vec![(
            ProviderId::Tavily,
            Mode::Keyless,
            u64::MAX,
            web::Outcome::Limited,
        )],
        ..Default::default()
    };
    settings::apply(&profile.dir, &cooldown).unwrap();
    let completed = engine
        .probe_provider(ProviderId::Tavily, &|| lease.has_changed().is_ok())
        .await;
    assert!(completed.is_ok());
    let state_path = profile.dir.join(settings::STATE_FILE);
    let before = std::fs::read(&state_path).unwrap();

    // Drive the same outbound gate without the outer cancellation select,
    // so it—not cancellation of the pending HTTP future—must stop the
    // second MCP message when the first reply arrives after sign-out.
    let pending = tokio::spawn(async move {
        engine
            .probe_provider(ProviderId::Exa, &|| lease.has_changed().is_ok())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), f.initialized.notified())
        .await
        .unwrap();
    web.session_ended().await;
    web.session_bound(&base, "alice", &session, 2).await;
    f.release_init.notify_one();
    assert!(matches!(pending.await.unwrap(), Err(web::BINDING_ENDED)));
    assert_eq!(f.mcp_sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        web.commit_test(&base, "alice", 1, &profile, ProviderId::Tavily, completed)
            .await
            .unwrap_err()
            .code,
        "no_session"
    );
    assert_eq!(std::fs::read(&state_path).unwrap(), before);
}

#[tokio::test]
async fn desktop_off_persists_and_unpairs_with_broken_endpoints_and_pauses() {
    let (base, f) = fixture().await;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut web = WebSearch::new(root, true, Duration::from_millis(25), Events::new(root));
    // The same rejected configuration produced by an invalid environment
    // override, without mutating process-wide environment for other tests.
    web.endpoints = Endpoints::with_overrides(|_| Some("not a URL".into()))
        .map_err(|_| "fixture invalid endpoints".into());
    let profile = web.profile(&base, "alice").unwrap();
    let prefs = settings::Settings {
        notice_acknowledged: true,
        ..Default::default()
    };
    settings::save(&profile.dir, &prefs).unwrap();
    let paused = profile.dir.join(settings::STATE_FILE);
    crate::fsutil::write_private(&paused, b"{broken").unwrap();
    let saved = Binding::new(
        &base,
        &owner("alice"),
        Some("wd_fixture".into()),
        Secret::new(format!("metro_pc_{:064x}", 1)),
    )
    .unwrap();
    binding::save(&profile.dir, &saved).unwrap();
    let session = Secret::new("fixture-session");
    web.session_bound(&base, "alice", &session, 1).await;
    let before = web.state(&base, "alice").await.unwrap();
    assert!(before["endpoints_error"].is_string());
    assert!(before["settings_error"].is_string());
    assert_eq!(before["device"]["paired"], true);

    let after = web
        .update(&base, "alice", 1, &session, Some(false), None, None)
        .await
        .unwrap();
    assert_eq!(after["enabled"], false);
    assert_eq!(after["device"]["worker"], "stopped");
    assert_eq!(after["device"]["paired"], false);
    assert_eq!(f.deleted.load(Ordering::SeqCst), 1);
    assert!(binding::load(&profile.dir).unwrap().is_none());
    assert_eq!(std::fs::read(&paused).unwrap(), b"{broken");
    settings::update_prefs(&profile.dir, false, |s| {
        assert!(!s.enabled);
        assert!(s.notice_acknowledged);
        Ok(())
    })
    .unwrap();
    assert_eq!(f.mcp_sends.load(Ordering::SeqCst), 0);
}
