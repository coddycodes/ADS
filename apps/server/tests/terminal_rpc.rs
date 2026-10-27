use coddy_studio_server::terminal::TerminalLaunchCommand;
use serde_json::Value;

#[test]
fn terminal_wire_fixture_keeps_effect_rpc_method_and_stream_names() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/terminal-rpc-wire.json"))
        .expect("terminal RPC fixture should be valid JSON");

    assert_eq!(fixture["methods"].as_array().map(Vec::len), Some(7));
    assert_eq!(fixture["methods"][0], "terminal.open");
    assert_eq!(fixture["methods"][1], "terminal.attach");
    assert_eq!(fixture["streams"][0], "subscribeTerminalEvents");
    assert_eq!(fixture["streams"][1], "subscribeTerminalMetadata");
    assert_eq!(fixture["attachSnapshot"]["type"], "snapshot");
    assert_eq!(
        fixture["attachSnapshot"]["snapshot"]["terminalId"],
        "term-1"
    );
}

#[test]
fn observer_hint_wire_fixture_covers_provider_activity_launch_commands() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/terminal-rpc-wire.json"))
        .expect("terminal RPC fixture should be valid JSON");

    assert_eq!(
        fixture["commands"]["codexActivity"]["activity"],
        serde_json::json!({
            "driverKind": "codex",
            "providerInstanceId": "codex_personal",
        })
    );
    assert!(
        fixture["commands"]["withoutActivity"]
            .get("activity")
            .is_none()
    );
    assert_eq!(
        fixture["commands"]["mergedEnvLaunch"]["env"],
        serde_json::json!({
            "COMMAND_ONLY": "command",
            "RUNTIME_ONLY": "runtime",
            "SHARED": "runtime",
        })
    );
    assert!(
        fixture["commands"]["codexActivity"]["activity"]
            .get("token")
            .is_none()
    );

    let hinted: TerminalLaunchCommand =
        serde_json::from_value(fixture["commands"]["codexActivity"].clone())
            .expect("valid Codex activity command decodes");
    let activity = hinted.activity.expect("activity hint");
    assert_eq!(activity.driver_kind, "codex");
    assert_eq!(activity.provider_instance_id, "codex_personal");

    let unhinted: TerminalLaunchCommand =
        serde_json::from_value(fixture["commands"]["withoutActivity"].clone())
            .expect("command without activity decodes");
    assert!(unhinted.activity.is_none());
    assert!(
        serde_json::to_value(&unhinted)
            .expect("command serializes")
            .get("activity")
            .is_none()
    );

    for invalid in fixture["commands"]["invalidActivities"]
        .as_array()
        .expect("invalid activity fixtures")
        .iter()
        .skip(3)
    {
        let mut command = fixture["commands"]["withoutActivity"].clone();
        command["activity"] = invalid.clone();
        assert!(
            serde_json::from_value::<TerminalLaunchCommand>(command).is_err(),
            "strict activity wire decoder must reject {invalid:?}"
        );
    }
}
