use std::collections::HashSet;
use std::path::Path;

use base64::Engine;
use rig_core::catalog::Catalog;
use rig_core::providers::registry::ProviderId;

use super::catalog::{self, Adapter, Capability, Execution};
use super::secret::{MasterKey, OpenError};
use super::{normalize_base_url, parse_allowed_base_urls};

fn key_text(byte: u8) -> String {
    base64::engine::general_purpose::STANDARD.encode([byte; 32])
}

// ---- Sealed keys ----

#[test]
fn a_sealed_key_opens_only_for_its_owner_and_row() {
    let master = MasterKey::parse(&key_text(7)).unwrap();
    let sealed = master.seal("alice", "pc_1", "sk-secret-value").unwrap();
    assert!(
        !sealed.windows(15).any(|w| w == b"sk-secret-value"),
        "plaintext in the sealed blob"
    );
    assert_eq!(
        master.open("alice", "pc_1", &sealed).unwrap(),
        "sk-secret-value"
    );
    // Moved to another user or another row: refused.
    assert_eq!(
        master.open("bob", "pc_1", &sealed),
        Err(OpenError::Rejected)
    );
    assert_eq!(
        master.open("alice", "pc_2", &sealed),
        Err(OpenError::Rejected)
    );
    // No ambiguity between (user, connection) splits.
    let ab = master.seal("a", "bc", "k").unwrap();
    assert_eq!(master.open("ab", "c", &ab), Err(OpenError::Rejected));
    // Another master key: refused.
    let other = MasterKey::parse(&key_text(8)).unwrap();
    assert_eq!(
        other.open("alice", "pc_1", &sealed),
        Err(OpenError::Rejected)
    );
    // Tampering: refused.
    let mut bad = sealed.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(master.open("alice", "pc_1", &bad), Err(OpenError::Rejected));
    assert_eq!(
        master.open("alice", "pc_1", b"v1short"),
        Err(OpenError::Malformed)
    );
    assert_eq!(
        master.open("alice", "pc_1", b"plaintext"),
        Err(OpenError::Malformed)
    );
}

#[test]
fn every_seal_uses_a_fresh_nonce() {
    let master = MasterKey::parse(&key_text(1)).unwrap();
    let seals: HashSet<Vec<u8>> = (0..64)
        .map(|_| master.seal("alice", "pc_1", "same").unwrap())
        .collect();
    assert_eq!(seals.len(), 64);
    let nonces: HashSet<Vec<u8>> = seals.iter().map(|s| s[2..26].to_vec()).collect();
    assert_eq!(nonces.len(), 64);
}

#[test]
fn master_keys_must_be_32_bytes_of_base64_from_one_source() {
    let db = Path::new("metro.sqlite3");
    assert!(MasterKey::parse("not base64!").is_err());
    let short = base64::engine::general_purpose::STANDARD.encode([0u8; 31]);
    let err = MasterKey::parse(&short).unwrap_err().to_string();
    assert!(err.contains("32 bytes"), "{err}");
    assert!(!err.contains(&short));
    assert!(MasterKey::parse(&format!("  {}\n", key_text(3))).is_ok());
    assert_eq!(
        format!("{:?}", MasterKey::parse(&key_text(3)).unwrap()),
        "MasterKey(<redacted>)"
    );

    assert!(MasterKey::from_sources(None, None, db).unwrap().is_none());
    assert!(
        MasterKey::from_sources(Some(key_text(1)), None, db)
            .unwrap()
            .is_some()
    );
    let both = MasterKey::from_sources(Some(key_text(1)), Some("/nonexistent".into()), db);
    assert!(both.unwrap_err().to_string().contains("only one"));
    // A bad inline key never falls back to anything else.
    assert!(MasterKey::from_sources(Some("bad".into()), None, db).is_err());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key.b64");
    std::fs::write(&path, format!("{}\n", key_text(9))).unwrap();
    let from_file = MasterKey::from_sources(None, Some(path.display().to_string()), db)
        .unwrap()
        .unwrap();
    let sealed = from_file.seal("u", "c", "x").unwrap();
    assert_eq!(
        MasterKey::parse(&key_text(9))
            .unwrap()
            .open("u", "c", &sealed)
            .unwrap(),
        "x"
    );
    let missing =
        MasterKey::from_sources(None, Some(dir.path().join("no").display().to_string()), db);
    assert!(missing.is_err());
    std::fs::write(&path, "garbage").unwrap();
    let garbage = MasterKey::from_sources(None, Some(path.display().to_string()), db);
    assert!(!format!("{:#}", garbage.unwrap_err()).contains("garbage"));
}

// ---- Base URLs ----

#[test]
fn allowed_base_urls_are_exact_plain_http_urls() {
    assert!(parse_allowed_base_urls(None).unwrap().is_empty());
    assert_eq!(
        parse_allowed_base_urls(Some(
            r#"["http://127.0.0.1:11434/v1/", "https://gw.example/v1", "https://gw.example/v1"]"#
        ))
        .unwrap(),
        ["http://127.0.0.1:11434/v1", "https://gw.example/v1"]
    );
    for bad in [
        r#""http://x""#,
        r#"["ftp://x/v1"]"#,
        r#"["http://user:pw@x/v1"]"#,
        r#"["http://x/v1?key=1"]"#,
        r#"["http://x/v1#f"]"#,
        r#"["/v1"]"#,
        r#"[""]"#,
        r#"[1]"#,
    ] {
        assert!(parse_allowed_base_urls(Some(bad)).is_err(), "{bad}");
    }
    assert_eq!(
        normalize_base_url(" https://a.example/v1// ").unwrap(),
        "https://a.example/v1"
    );
}

// ---- Catalog ----

#[test]
fn the_catalog_comes_from_the_compiled_registry() {
    let ids: Vec<&str> = catalog::entries().iter().map(|e| e.id.as_str()).collect();
    let unique: HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate ids: {ids:?}");
    for want in [
        "openai",
        "anthropic",
        "gcp.gemini",
        "deepseek",
        "openrouter",
        "ollama",
        "ollama/anthropic",
        "zai",
        "zai/anthropic",
        "minimax",
        "minimax/openai",
        "custom",
        "device-openai-oauth",
        "device-claude-code",
        "device-openai-compatible",
        "systemone",
        "custom-systemone",
        "openai-embeddings",
        "custom-embeddings",
        "device-systemone",
        "device-embeddings",
    ] {
        assert!(unique.contains(want), "{want} missing from {ids:?}");
    }
    // Adapters Metro cannot drive with an API key and tools are left out.
    for gone in ["chatgpt", "copilot", "hyperbolic", "mira", "perplexity"] {
        assert!(!unique.contains(gone), "{gone} should not be offered");
    }
    // Every registered entry names a real Rig selection and builds a model
    // offline, through the adapter's own completion wire.
    let mut registered = 0;
    for entry in catalog::entries() {
        match entry.adapter {
            Adapter::Registered(id) => {
                registered += 1;
                assert!(id.is_registered());
                assert!(ProviderId::all().any(|p| p == id));
                assert_eq!(entry.format, id.format().unwrap().as_str());
                assert_eq!(entry.requires_key, id.requires_credential());
                assert_eq!(entry.execution, Execution::Server);
                let config = id.config("test-key").unwrap();
                let config = match &entry.default_base_url {
                    Some(url) => {
                        assert!(url.starts_with("https://"), "{url}");
                        config
                    }
                    None => config.with_base_url("http://127.0.0.1:9/v1"),
                };
                let _model = config.completion_model(
                    "some-model",
                    rig_reqwest::shared(),
                    Catalog::builtin().facts(id, "some-model"),
                );
            }
            Adapter::Custom => assert_eq!(entry.id, catalog::CUSTOM),
            Adapter::SystemOne | Adapter::Embeddings => {
                assert_eq!(entry.execution, Execution::Server);
                assert_ne!(entry.capability, catalog::Capability::Chat);
            }
            Adapter::Device => {
                assert_eq!(entry.execution, Execution::Device);
                assert!(!entry.requires_key && entry.default_base_url.is_none());
            }
        }
    }
    assert!(registered >= 25, "only {registered} registered entries");
    // The device ids agree with the relay's.
    let devices: HashSet<&str> = catalog::entries()
        .iter()
        .filter(|e| e.is_device())
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(
        devices,
        crate::device_relay::DEVICE_PROVIDERS.into_iter().collect()
    );
    for id in crate::device_relay::DEVICE_PROVIDERS {
        assert!(catalog::is_device(id) && crate::device_relay::is_device_provider(id));
    }
    assert!(!catalog::is_device("openai") && !catalog::is_device("custom"));
    // Capabilities come from the adapter.
    for entry in catalog::entries() {
        let want = match entry.id.as_str() {
            "systemone" | "custom-systemone" | "device-systemone" => Capability::SystemOne,
            "openai-embeddings"
            | "custom-embeddings"
            | "device-embeddings"
            | "embeddinggemma-2"
            | "device-embeddinggemma-2" => Capability::Embeddings,
            _ => Capability::Chat,
        };
        assert_eq!(entry.capability, want, "{}", entry.id);
    }
    let systemone = catalog::find("systemone").unwrap();
    assert_eq!(
        systemone.default_base_url.as_deref(),
        Some("https://api.typesafe.ai/v1")
    );
    assert!(systemone.requires_key && systemone.lists_models);
    assert!(
        catalog::find("custom-systemone")
            .unwrap()
            .default_base_url
            .is_none()
    );
    assert!(catalog::is_subscription("device-openai-oauth"));
    assert!(!catalog::is_subscription("device-claude-code"));
}

#[test]
fn local_and_custom_endpoints_need_an_operator_allowlist() {
    let shown = |allowed: &[String]| -> HashSet<String> {
        catalog::view(allowed).into_iter().map(|e| e.id).collect()
    };
    let none = shown(&[]);
    for hidden in ["custom", "ollama", "llamacpp", "azure.openai"] {
        assert!(
            !none.contains(hidden),
            "{hidden} shown without an allowlist"
        );
    }
    assert!(none.contains("openai") && none.contains("device-claude-code"));
    for entry in catalog::view(&[]) {
        assert!(!entry.allow_custom_url);
    }
    let allowed = vec!["http://127.0.0.1:11434/v1".to_owned()];
    let some = shown(&allowed);
    assert!(some.contains("custom") && some.contains("ollama"));
    for entry in catalog::view(&allowed) {
        assert_eq!(
            entry.allow_custom_url,
            entry.execution == Execution::Server,
            "{}",
            entry.id
        );
    }
    let json = serde_json::to_value(catalog::view(&allowed)).unwrap();
    let openai = json
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "openai")
        .unwrap();
    assert_eq!(openai["default_base_url"], "https://api.openai.com/v1");
    assert_eq!(openai["execution"], "server");
    assert_eq!(openai["api"], "OpenAI Responses");
    let custom = json
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "custom")
        .unwrap();
    assert!(custom.get("default_base_url").is_none());
    assert_eq!(custom["requires_key"], false);
    let claude = json
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "device-claude-code")
        .unwrap();
    assert_eq!(claude["name"], "Claude (local CLI)");
    assert_eq!(claude["execution"], "device");
}

#[test]
fn model_ids_are_cleaned_and_bounded() {
    let ids = super::api::clean_model_ids(
        [
            "b",
            "a",
            "a",
            "",
            "has space",
            "x\u{0}",
            "qwen3:4b",
            "models/gemini-2.5-pro",
        ]
        .into_iter()
        .map(String::from)
        .chain((0..2_000).map(|i| format!("m{i:05}"))),
    );
    assert_eq!(&ids[..4], ["a", "b", "m00000", "m00001"]);
    assert!(ids.len() == super::MAX_LISTED_MODELS);
    assert!(super::api::valid_connection_id(
        "pc_0123456789abcdef0123456789abcdef"
    ));
    for bad in [
        "pc_",
        "pc_0123456789ABCDEF0123456789abcdef",
        "../x",
        "pc_0123456789abcdef0123456789abcde",
    ] {
        assert!(!super::api::valid_connection_id(bad), "{bad}");
    }
}

// ---- Capabilities, migration and sharing rules ----

fn old_schema_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE space_members (space_id TEXT, user_id TEXT, role TEXT, added_at INTEGER,
                                     PRIMARY KEY (space_id, user_id));
         CREATE TABLE provider_connections (
             id TEXT PRIMARY KEY NOT NULL, user_id TEXT NOT NULL, name TEXT NOT NULL,
             provider_id TEXT NOT NULL, base_url TEXT, model TEXT NOT NULL DEFAULT '',
             is_default INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
             sealed_key BLOB, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
             CHECK (is_default = 0 OR model <> ''));
         CREATE UNIQUE INDEX provider_connections_one_default
             ON provider_connections (user_id) WHERE is_default = 1;
         INSERT INTO provider_connections VALUES
             ('pc_00000000000000000000000000000001', 'alice', 'Old', 'openai', NULL, 'gpt',
              1, NULL, 1, 1);",
    )
    .unwrap();
    conn
}

fn row(id: u8, user: &str, provider: &str, model: &str) -> super::store::Row {
    let entry = catalog::find(provider).unwrap();
    super::store::Row {
        id: format!("pc_{:032x}", id),
        user_id: user.into(),
        name: provider.into(),
        provider_id: provider.into(),
        base_url: None,
        model: model.into(),
        is_default: false,
        sealed_key: None,
        capability: entry.capability,
        config_rev: 0,
    }
}

#[test]
fn old_connections_migrate_additively_as_chat() {
    use super::store;
    let mut conn = old_schema_db();
    store::init(&conn).unwrap();
    store::init(&conn).unwrap(); // idempotent
    let old = store::list(&conn, "alice").unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].capability, Capability::Chat);
    assert!(old[0].is_default && old[0].model == "gpt" && old[0].config_rev == 0);
    assert_eq!(
        store::default_for(&conn, "alice", Capability::Chat)
            .unwrap()
            .unwrap()
            .id,
        old[0].id
    );
    // One default per capability now, not per person.
    let mut s1 = row(2, "alice", "systemone", "jev-latest");
    s1.is_default = true;
    assert!(store::insert(&mut conn, &s1, 20).unwrap());
    assert!(
        store::default_for(&conn, "alice", Capability::Chat)
            .unwrap()
            .is_some()
    );
    assert!(
        store::default_for(&conn, "alice", Capability::SystemOne)
            .unwrap()
            .is_some()
    );
    // A model change bumps the revision; a rename doesn't.
    s1.name = "renamed".into();
    assert_eq!(store::update(&mut conn, &s1).unwrap(), Some(0));
    s1.model = "jev-2".into();
    assert_eq!(store::update(&mut conn, &s1).unwrap(), Some(1));
    // Deleting a connection drops its share.
    let share = store::Share {
        visibility: store::Visibility::Organization,
        space_ids: vec![],
    };
    assert!(store::set_share(&mut conn, "alice", &s1.id, &share).unwrap());
    assert!(!store::set_share(&mut conn, "bob", &s1.id, &share).unwrap());
    store::delete(&conn, "alice", &s1.id).unwrap().unwrap();
    let left: i64 = conn
        .query_row("SELECT COUNT(*) FROM provider_shares", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn patches_merge_only_their_fields_into_the_committed_row() {
    use super::store::{self, Patch, Patched};
    let mut conn = old_schema_db();
    store::init(&conn).unwrap();
    let mut c = row(3, "alice", "openai", "");
    c.sealed_key = Some(vec![1; 40]);
    assert!(store::insert(&mut conn, &c, 20).unwrap());
    let snapshot = store::get(&conn, "alice", &c.id).unwrap().unwrap();

    // Committed after the snapshot: a key rotation and a model.
    let rotate = Patch {
        sealed_key: Some(Some(vec![2; 40])),
        model: Some("gpt-new".into()),
        ..Patch::default()
    };
    let Patched::Saved(rotated) = store::patch(&mut conn, "alice", &c.id, &rotate, None).unwrap()
    else {
        panic!("rotation not saved");
    };
    assert_eq!(rotated.config_rev, snapshot.config_rev + 1);

    // A rename and a default flag prepared from the snapshot keep both.
    let flag = Patch {
        name: Some("Renamed".into()),
        is_default: Some(true),
        ..Patch::default()
    };
    let Patched::Saved(saved) = store::patch(&mut conn, "alice", &c.id, &flag, None).unwrap()
    else {
        panic!("rename not saved");
    };
    assert_eq!(saved.name, "Renamed");
    assert!(saved.is_default);
    assert_eq!(saved.sealed_key, Some(vec![2; 40]));
    assert_eq!(saved.model, "gpt-new");
    assert_eq!(saved.config_rev, rotated.config_rev);

    // The default check sees the committed row, not the caller's.
    let clear = Patch {
        model: Some(String::new()),
        ..Patch::default()
    };
    assert!(matches!(
        store::patch(&mut conn, "alice", &c.id, &clear, None).unwrap(),
        Patched::DefaultNeedsModel
    ));
    // Endpoint changes are compare-and-set only.
    let endpoint = Patch {
        base_url: Some("https://example.invalid/v1".into()),
        ..Patch::default()
    };
    assert!(matches!(
        store::patch(&mut conn, "alice", &c.id, &endpoint, None).unwrap(),
        Patched::Changed
    ));
    assert!(matches!(
        store::patch(&mut conn, "alice", &c.id, &endpoint, Some(&snapshot)).unwrap(),
        Patched::Changed
    ));
    assert!(matches!(
        store::patch(&mut conn, "bob", &c.id, &flag, None).unwrap(),
        Patched::Missing
    ));
    assert_eq!(store::get(&conn, "alice", &c.id).unwrap().unwrap(), saved);
}

#[test]
fn presets_run_only_on_the_callers_own_connection() {
    use super::{sharing, store};
    let mut conn = old_schema_db();
    store::init(&conn).unwrap();
    let preset = row(10, "alice", "device-openai-oauth", "gpt-5.5");
    store::insert(&mut conn, &preset, 20).unwrap();
    let org = store::Share {
        visibility: store::Visibility::Organization,
        space_ids: vec![],
    };
    store::set_share(&mut conn, "alice", &preset.id, &org).unwrap();
    // Bob has no eligible connection: a clear error, never Alice's account.
    assert_eq!(
        sharing::authorize(&conn, "bob", None, &preset.id).unwrap_err(),
        super::UseError::PresetNeedsOwnConnection("gpt-5.5".into())
    );
    // A connection of another provider or capability does not count.
    store::insert(&mut conn, &row(11, "bob", "anthropic", "claude"), 20).unwrap();
    store::insert(&mut conn, &row(12, "bob", "systemone", "jev"), 20).unwrap();
    assert!(sharing::authorize(&conn, "bob", None, &preset.id).is_err());
    // His API-billed OpenAI key is never picked for him.
    let key = row(13, "bob", "openai", "gpt-5.5");
    store::insert(&mut conn, &key, 20).unwrap();
    assert!(matches!(
        sharing::authorize(&conn, "bob", None, &preset.id),
        Err(super::UseError::PresetNeedsOwnConnection(_))
    ));
    assert!(
        sharing::preset_target(&conn, "bob", "gpt-5.5")
            .unwrap()
            .is_none()
    );
    // Only his own Sign in with ChatGPT; one already on that model first.
    let other = row(15, "bob", "device-openai-oauth", "gpt-4");
    store::insert(&mut conn, &other, 20).unwrap();
    let own = row(14, "bob", "device-openai-oauth", "gpt-5.5");
    store::insert(&mut conn, &own, 20).unwrap();
    assert_eq!(
        sharing::authorize(&conn, "bob", None, &preset.id)
            .unwrap()
            .row
            .id,
        own.id
    );
    let a = sharing::authorize(&conn, "bob", None, &preset.id).unwrap();
    assert_eq!(a.row.user_id, "bob");
    assert_eq!(a.row.model, "gpt-5.5");
    assert!(matches!(a.access, sharing::Access::Preset { .. }));
    // Its owner still uses it as their own; the relay never lends it.
    assert_eq!(
        sharing::authorize(&conn, "alice", None, &preset.id)
            .unwrap()
            .access,
        sharing::Access::Own
    );
    let dc = crate::device_relay::DeviceConnection {
        id: preset.id.clone(),
        owner_id: "alice".into(),
        provider: preset.provider_id.clone(),
    };
    assert!(!super::device_shared_use_allowed(&conn, &dc, "bob", None).unwrap());
}

#[test]
fn the_relay_lends_shared_devices_only_in_scope() {
    use super::store;
    let mut conn = old_schema_db();
    store::init(&conn).unwrap();
    let dev = row(20, "alice", "device-systemone", "jev");
    store::insert(&mut conn, &dev, 20).unwrap();
    let dc = crate::device_relay::DeviceConnection {
        id: dev.id.clone(),
        owner_id: "alice".into(),
        provider: dev.provider_id.clone(),
    };
    let allowed = |conn: &rusqlite::Connection, user: &str, space: Option<&str>| {
        super::device_shared_use_allowed(conn, &dc, user, space).unwrap()
    };
    assert!(!allowed(&conn, "bob", None));
    conn.execute_batch(
        "INSERT INTO space_members VALUES ('s-1', 'alice', 'owner', 0), ('s-1', 'bob', 'member', 0);",
    )
    .unwrap();
    let share = store::Share {
        visibility: store::Visibility::Spaces,
        space_ids: vec!["s-1".into()],
    };
    store::set_share(&mut conn, "alice", &dev.id, &share).unwrap();
    assert!(allowed(&conn, "bob", Some("s-1")));
    assert!(!allowed(&conn, "bob", None));
    assert!(!allowed(&conn, "bob", Some("s-2")));
    assert!(!allowed(&conn, "carol", Some("s-1")));
    // The owner is never "borrowing".
    assert!(!allowed(&conn, "alice", Some("s-1")));
    conn.execute("DELETE FROM space_members WHERE user_id = 'alice'", [])
        .unwrap();
    assert!(!allowed(&conn, "bob", Some("s-1")));
}

#[test]
fn only_the_embeddinggemma_adapters_take_media() {
    let media: HashSet<&str> = catalog::entries()
        .iter()
        .filter(|e| e.media_inputs)
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(
        media,
        HashSet::from([catalog::EMBEDDINGGEMMA, catalog::DEVICE_EMBEDDINGGEMMA])
    );
    for id in [catalog::EMBEDDINGGEMMA, catalog::DEVICE_EMBEDDINGGEMMA] {
        let e = catalog::find(id).unwrap();
        assert_eq!(e.capability, Capability::Embeddings);
        assert_eq!(e.default_model, Some("google/embeddinggemma-2"));
        assert_eq!(e.modalities, ["text", "image", "audio", "video"]);
        assert!(e.lists_models && !e.requires_key);
    }
    for e in catalog::entries().iter().filter(|e| !e.media_inputs) {
        assert!(
            e.modalities.is_empty() && e.default_model.is_none(),
            "{}",
            e.id
        );
    }
    // The server adapter runs only on an endpoint the operator approved.
    let server = catalog::find(catalog::EMBEDDINGGEMMA).unwrap();
    assert!(matches!(server.adapter, Adapter::Embeddings));
    assert_eq!(server.execution, Execution::Server);
    assert!(server.default_base_url.is_none());
    assert!(
        !catalog::view(&[])
            .iter()
            .any(|v| v.id == catalog::EMBEDDINGGEMMA)
    );
    let view = catalog::view(&["http://127.0.0.1:8090/v1".to_owned()]);
    let v = view
        .iter()
        .find(|v| v.id == catalog::EMBEDDINGGEMMA)
        .unwrap();
    assert!(v.media_inputs && v.allow_custom_url);
    let json = serde_json::to_value(&view).unwrap();
    let text = json
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "custom-embeddings")
        .unwrap();
    assert_eq!(text["media_inputs"], false);
    assert!(text.get("modalities").is_none());
}

#[test]
fn only_a_smaller_share_counts_as_narrowed() {
    use super::store::{Share, Visibility, narrowed};
    let share = |v: Visibility, s: &[&str]| Share {
        visibility: v,
        space_ids: s.iter().map(|x| (*x).to_owned()).collect(),
    };
    let (p, o, sp) = (
        Visibility::Personal,
        Visibility::Organization,
        Visibility::Spaces,
    );
    for (before, after, want) in [
        (share(p, &[]), share(o, &[]), false),
        (share(p, &[]), share(p, &[]), false),
        (share(o, &[]), share(o, &[]), false),
        (share(o, &[]), share(sp, &["s1"]), true),
        (share(o, &[]), share(p, &[]), true),
        (share(sp, &["s1"]), share(o, &[]), false),
        (share(sp, &["s1"]), share(sp, &["s1", "s2"]), false),
        (share(sp, &["s1", "s2"]), share(sp, &["s2"]), true),
        (share(sp, &["s1"]), share(sp, &["s2"]), true),
        (share(sp, &["s1"]), share(p, &[]), true),
    ] {
        assert_eq!(narrowed(&before, &after), want, "{before:?} -> {after:?}");
    }
}
