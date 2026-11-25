use std::sync::Arc;

use serde_json::{Value, json};

use super::grant::{Denied, Grant};
use super::prefs::{self, AccessBody};
use super::{Ctx, budget, tools};
use crate::auth::{AuthFuture, HumanAuth};
use crate::db::{Activity, Db};
use crate::{AppState, Config, Limits};

struct NoAuth;

impl HumanAuth for NoAuth {
    fn identity<'a>(&'a self, _bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async { Err(crate::auth::AuthFailure::Rejected("no".into())) })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

async fn state() -> AppState {
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model: None,
        limits: Limits::default(),
        org_admins: Vec::new(),
    };
    AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(NoAuth))
        .await
        .unwrap()
}

/// A Space of alice's with bob in it; its id.
fn space(db: &Db, name: &str) -> String {
    let Ok(s) = db.create_space("alice", name, 10) else {
        panic!("creating a Space");
    };
    db.add_member(&s.id, "bob", 10).ok().unwrap();
    s.id
}

fn ctx(state: &AppState, user: &str) -> Ctx {
    Ctx {
        state: state.clone(),
        user: user.to_owned(),
        name: user.to_owned(),
        turn: Activity::chat(),
        budget: budget::Budget::default(),
        own_text: Arc::new(Vec::new()),
        previous_message_at: None,
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(60),
    }
}

fn shown(db: &Db, user: &str) {
    let body = AccessBody {
        notice_shown: Some(true),
        ..Default::default()
    };
    prefs::put(&mut db.lock(), user, &body).unwrap().unwrap();
}

#[tokio::test]
async fn the_grant_needs_switch_notice_membership_and_no_exclusion() {
    let st = state().await;
    let db = &st.db;
    let sid = space(db, "Design");
    let g = Grant::new("bob");
    // On by default, but nothing before the notice was shown.
    assert!(prefs::switch_in(&db.lock(), "bob").unwrap().enabled);
    assert_eq!(g.check(&db.lock(), &sid).unwrap(), Err(Denied::Off));
    shown(db, "bob");
    assert!(g.check(&db.lock(), &sid).unwrap().is_ok());
    assert_eq!(
        Grant::new("carol").check(&db.lock(), &sid).unwrap(),
        Err(Denied::Off)
    );
    shown(db, "carol");
    assert_eq!(
        Grant::new("carol").check(&db.lock(), &sid).unwrap(),
        Err(Denied::NoSpace)
    );
    let exclude = AccessBody {
        excluded_space_ids: Some(vec![sid.clone()]),
        ..Default::default()
    };
    prefs::put(&mut db.lock(), "bob", &exclude)
        .unwrap()
        .unwrap();
    assert_eq!(g.check(&db.lock(), &sid).unwrap(), Err(Denied::NoSpace));
    assert_eq!(g.spaces(&db.lock()).unwrap().unwrap(), Vec::new());
    assert_eq!(
        g.spaces_among(&db.lock(), Some(std::slice::from_ref(&sid)))
            .unwrap(),
        Err(Denied::NoSpace)
    );
    let off = AccessBody {
        enabled: Some(false),
        ..Default::default()
    };
    prefs::put(&mut db.lock(), "bob", &off).unwrap().unwrap();
    assert_eq!(g.spaces(&db.lock()).unwrap(), Err(Denied::Off));
    // Excluding a Space one isn't in is refused, whole.
    assert_eq!(
        prefs::put(&mut db.lock(), "carol", &exclude).unwrap(),
        Err(prefs::PutError::NotFound)
    );
}

#[tokio::test]
async fn leaving_or_deleting_a_space_purges_exclusions() {
    let st = state().await;
    let db = &st.db;
    let a = space(db, "A");
    let b = space(db, "B");
    let both = AccessBody {
        excluded_space_ids: Some(vec![a.clone(), b.clone()]),
        ..Default::default()
    };
    prefs::put(&mut db.lock(), "bob", &both).unwrap().unwrap();
    let rows = |db: &Db| -> i64 {
        db.lock()
            .query_row("SELECT COUNT(*) FROM meta_space_exclusions", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    assert_eq!(rows(db), 2);
    db.remove_member(&a, "bob").unwrap();
    assert_eq!(rows(db), 1);
    assert!(matches!(
        db.delete_space(&b, "alice", "B").unwrap(),
        crate::spaces::Deleted::Done { .. }
    ));
    assert_eq!(rows(db), 0);
}

#[tokio::test]
async fn settings_explain_never_shows_endpoints_keys_or_ids() {
    let st = state().await;
    let db = &st.db;
    space(db, "Design");
    shown(db, "bob");
    let secret = "https://secret-endpoint.example/v1";
    db.lock()
        .execute(
            "INSERT INTO provider_connections
               (id, user_id, name, provider_id, base_url, model, is_default, sealed_key,
                capability, created_at, updated_at)
             VALUES ('pc_0123456789abcdef0123456789abcdef', 'bob', 'Work model', 'custom',
                     ?1, 'm', 0, X'0102030405', 'system_one', 1, 1)",
            [secret],
        )
        .unwrap();
    // Someone else's connection is never mentioned either.
    db.lock()
        .execute(
            "INSERT INTO provider_connections
               (id, user_id, name, provider_id, base_url, model, is_default, capability,
                created_at, updated_at)
             VALUES ('pc_ffffffffffffffffffffffffffffffff', 'alice', 'Alice private', 'custom',
                     'https://alice.example', 'm', 0, 'chat', 1, 1)",
            [],
        )
        .unwrap();
    let out = tools::settings_explain(&ctx(&st, "bob"), Value::Null)
        .await
        .unwrap();
    let text = out.value.to_string();
    for leak in [
        "secret-endpoint",
        "pc_0123",
        "Alice private",
        "alice.example",
        "device",
        "custom",
    ] {
        assert!(!text.contains(leak), "{leak} in {text}");
    }
    assert_eq!(out.value["connections"][0]["name"], "Work model");
    assert_eq!(out.value["spaces_in_chat"]["usable_spaces"], 1);
}

#[tokio::test]
async fn output_fits_the_turn_budget_by_dropping_list_items() {
    let st = state().await;
    let c = ctx(&st, "bob");
    let items: Vec<Value> = (0..400)
        .map(|i| json!({"n": i, "pad": "x".repeat(400)}))
        .collect();
    let mut first = json!({"items": items.clone()});
    let text = tools::render(&c, &mut first);
    assert!(text.len() <= st.config.limits.max_tool_output_to_model);
    assert_eq!(first["truncated"], true);
    assert!(text.ends_with("\">"), "a closed block");
    // The rest of the turn's 96 KiB, then nothing.
    let mut second = json!({"items": items.clone()});
    let text = tools::render(&c, &mut second);
    assert!(text.len() <= budget::MAX_OUTPUT_BYTES - st.config.limits.max_tool_output_to_model + 1);
    assert!(c.budget.admit(budget::Kind::Read).is_err());
}
