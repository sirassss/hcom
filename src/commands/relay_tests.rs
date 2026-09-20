use super::*;
use crate::hooks::test_helpers::isolated_test_env;
use serial_test::serial;

fn fake_psk() -> [u8; 32] {
    [0x44; 32]
}

#[test]
fn test_encode_decode_public_broker_token() {
    let relay_id = uuid::Uuid::new_v4().to_string();
    let broker = format!("mqtts://{}:{}", DEFAULT_BROKERS[0].0, DEFAULT_BROKERS[0].1);
    let psk = fake_psk();
    let token = encode_join_token(&relay_id, &broker, &psk).unwrap();
    let decoded = decode_join_token(&token).unwrap();
    assert_eq!(decoded.relay_id, relay_id);
    assert_eq!(decoded.broker_url, broker);
    assert_eq!(decoded.psk, Some(psk));
}

#[test]
fn test_encode_decode_private_broker_token() {
    let relay_id = uuid::Uuid::new_v4().to_string();
    let broker = "mqtts://my-broker.example.com:8883";
    let psk = fake_psk();
    let token = encode_join_token(&relay_id, broker, &psk).unwrap();
    let decoded = decode_join_token(&token).unwrap();
    assert_eq!(decoded.relay_id, relay_id);
    assert_eq!(decoded.broker_url, broker);
    assert_eq!(decoded.psk, Some(psk));
}

#[test]
fn test_decode_invalid_token() {
    assert!(decode_join_token("not-a-token").is_none());
    assert!(decode_join_token("").is_none());
}

#[test]
fn test_update_toml_key_existing() {
    let content = "[relay]\nurl = \"\"\nid = \"\"\nenabled = false\n[other]\nfoo = 1\n";
    let result = update_toml_key(content, "relay_enabled", "true");
    assert!(result.contains("enabled = true"));
    assert!(result.contains("foo = 1"));
}

#[test]
fn test_update_toml_key_new() {
    let content = "[other]\nfoo = 1\n";
    let result = update_toml_key(content, "relay_enabled", "true");
    // Should create [relay] section with enabled = true
    let doc: toml_edit::DocumentMut = result.parse().unwrap();
    assert_eq!(doc["relay"]["enabled"].as_bool(), Some(true));
    assert_eq!(doc["other"]["foo"].as_integer(), Some(1));
}

#[test]
fn test_parse_broker_flags() {
    let argv: Vec<String> = vec![
        "--broker".into(),
        "mqtts://host:8883".into(),
        "--password".into(),
        "secret".into(),
        "other".into(),
    ];
    let (broker, auth, remaining) = parse_broker_flags(&argv);
    assert_eq!(broker.as_deref(), Some("mqtts://host:8883"));
    assert_eq!(auth.as_deref(), Some("secret"));
    assert_eq!(remaining, vec!["other"]);
}

#[test]
#[serial]
fn test_persist_relay_config_clears_stale_token_when_password_omitted() {
    let _ = isolated_test_env();
    let psk_b64 = relay::encode_psk(&fake_psk());
    let contents = render_relay_config_content(
        "[relay]\nurl = \"mqtt://old:1883\"\nid = \"old-id\"\ntoken = \"stale-secret\"\npsk = \"old-psk\"\nenabled = true\n",
        "new-id",
        "mqtt://127.0.0.1:1",
        None,
        &psk_b64,
    );
    let doc: toml_edit::DocumentMut = contents.parse().unwrap();
    assert_eq!(doc["relay"]["id"].as_str(), Some("new-id"));
    assert_eq!(doc["relay"]["url"].as_str(), Some("mqtt://127.0.0.1:1"));
    assert_eq!(doc["relay"]["token"].as_str(), Some(""));
    assert_eq!(doc["relay"]["psk"].as_str(), Some(psk_b64.as_str()));
    assert_eq!(doc["relay"]["enabled"].as_bool(), Some(true));
}

#[test]
fn test_legacy_token_rejected_in_connect_decode() {
    // A v0x01 (plaintext) token decodes to psk=None; relay_connect refuses
    // it instead of writing config.
    let relay_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
    let broker = format!("mqtts://{}:{}", DEFAULT_BROKERS[0].0, DEFAULT_BROKERS[0].1);
    let legacy = relay::token::encode_join_token(relay_id, &broker, None).unwrap();
    let decoded = decode_join_token(&legacy).unwrap();
    assert!(decoded.psk.is_none());
}

#[test]
#[serial]
fn test_relay_off_all_disables_local_relay_without_peers() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let cfg = crate::config::HcomConfig {
        relay: "mqtts://broker.emqx.io:8883".to_string(),
        relay_id: "relay-1".to_string(),
        relay_psk: relay::encode_psk(&fake_psk()),
        relay_enabled: true,
        ..Default::default()
    };
    crate::config::save_toml_config(&cfg, None).unwrap();

    let db = HcomDb::open().unwrap();
    let args = RelayArgs {
        args: vec!["off".to_string(), "--all".to_string()],
    };
    assert_eq!(cmd_relay(&db, &args, None), 0);

    let updated = crate::config::HcomConfig::load(None).unwrap();
    assert!(!updated.relay_enabled);
}

#[test]
#[serial]
fn test_relay_push_subcommand_exists() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let db = HcomDb::open().unwrap();
    let args = RelayArgs {
        args: vec!["push".to_string()],
    };
    assert_eq!(cmd_relay(&db, &args, None), 0);
}

#[test]
#[serial]
fn test_relay_on_rejects_invalid_stored_config_without_enabling() {
    let (_dir, _hcom_dir, _home, _guard) = isolated_test_env();
    let mut content = String::new();
    content = update_toml_key(&content, "relay_id", "\"relay-1\"");
    content = update_toml_key(&content, "relay", "\"\"");
    content = update_toml_key(&content, "relay_psk", "\"not-valid-base64\"");
    content = update_toml_key(&content, "relay_enabled", "false");
    std::fs::write(crate::paths::config_toml_path(), content).unwrap();

    let db = HcomDb::open().unwrap();
    let args = RelayArgs {
        args: vec!["on".to_string()],
    };
    assert_eq!(cmd_relay(&db, &args, None), 1);

    let updated = crate::config::HcomConfig::load(None).unwrap();
    assert!(!updated.relay_enabled);
}
