use super::*;

/// Read capabilities for one chat context without publishing environment-wide inventory.
pub(crate) async fn discover_capabilities(
    settings: &Value,
    instance_id: &str,
    cwd: &Path,
    cancellation: &CancellationToken,
) -> Result<Value, String> {
    if !cwd.is_absolute()
        || !tokio::fs::metadata(cwd)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err("Select an existing absolute workspace directory.".to_owned());
    }
    let mut definition = definitions(settings)
        .into_iter()
        .find(|definition| definition.instance_id == instance_id)
        .filter(|definition| definition.enabled && definition.available)
        .ok_or_else(|| {
            "The selected provider is unavailable or disabled. Check provider settings.".to_owned()
        })?;
    if cancellation.is_cancelled() {
        return Err("Skill discovery was cancelled. Retry to reload the catalog.".to_owned());
    }
    let mut capabilities = if definition.driver == "cursor" {
        let discovered = tokio::select! {
            () = cancellation.cancelled() => return Err("Skill discovery was cancelled.".to_owned()),
            result = timeout(CURSOR_DISCOVERY_TIMEOUT, cursor::discover_workspace_capabilities_with_environment(cwd, &definition.environment)) =>
                result.map_err(|_| "Skill discovery timed out. Retry to reload the catalog.".to_owned())?,
        };
        ProviderCapabilities {
            slash_commands: discovered.slash_commands,
            skills: discovered.skills,
            agents: discovered.agents,
            issues: discovered.issues,
        }
    } else if definition.driver == "opencode" && definition.endpoint.is_some() {
        tokio::select! {
            () = cancellation.cancelled() => None,
            result = opencode_capabilities(definition.endpoint.as_deref().unwrap(), definition.server_password.as_deref(), cwd, None) => result,
        }.ok_or_else(|| "Could not load OpenCode skills. Check the server connection and retry.".to_owned())?
    } else {
        let executable = resolve_provider_executable_with_environment(
            &definition.binary_path,
            definition
                .environment
                .iter()
                .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
        )
        .ok_or_else(|| {
            "The provider executable was not found. Check provider settings.".to_owned()
        })?;
        match definition.driver.as_str() {
            "codex" => {
                let config = settings
                    .get("providerInstances")
                    .and_then(|instances| instances.get(instance_id))
                    .and_then(|instance| instance.get("config"));
                let layout = codex::resolve_codex_home_layout(
                    string_setting(config, "homePath"),
                    string_setting(config, "shadowHomePath"),
                    dirs::home_dir()
                        .as_deref()
                        .unwrap_or_else(|| Path::new(".")),
                );
                codex::materialize_codex_shadow_home(&layout)
                    .await
                    .map_err(|error| error.to_string())?;
                if let Some(home) = layout.effective_home_path {
                    definition.environment.retain(|(name, _)| {
                        if cfg!(windows) {
                            !name.to_string_lossy().eq_ignore_ascii_case("CODEX_HOME")
                        } else {
                            name != "CODEX_HOME"
                        }
                    });
                    definition
                        .environment
                        .push(("CODEX_HOME".into(), home.into_os_string()));
                }
                codex_capabilities(&executable, cwd, &definition.environment, cancellation).await?
            }
            "claudeAgent" => {
                let metadata = probe_claude_metadata_with_cancellation(
                    &executable,
                    cwd,
                    &definition.environment,
                    &[],
                    cancellation,
                )
                .await
                .ok_or_else(|| {
                    "Could not load Claude skills. Check provider settings and retry.".to_owned()
                })?;
                let mut capabilities = metadata.capabilities;
                if !metadata.skills_loaded {
                    capabilities.issues.push(
                        "Claude could not reload skills. Retry to load the full catalog."
                            .to_owned(),
                    );
                }
                let skills = capabilities
                    .skills
                    .iter()
                    .filter_map(|skill| skill["name"].as_str())
                    .collect::<HashSet<_>>();
                capabilities.slash_commands.retain(|command| {
                    !command["name"]
                        .as_str()
                        .is_some_and(|name| skills.contains(name))
                });
                capabilities
            }
            "opencode" => with_local_opencode(
                &executable,
                cwd,
                &definition.environment,
                cancellation,
                |endpoint, password, protocol| async move {
                    opencode_capabilities(&endpoint, Some(&password), cwd, Some(protocol)).await
                },
            )
            .await
            .ok_or_else(|| {
                "Could not load OpenCode skills. Check provider settings and retry.".to_owned()
            })?,
            _ => return Err("Skill discovery is not supported for this provider.".to_owned()),
        }
    };
    capabilities.slash_commands = merge_slash_commands(
        capabilities.slash_commands,
        built_in_slash_commands(&definition.driver),
    );
    serde_json::to_value(capabilities).map_err(|error| error.to_string())
}

async fn codex_capabilities(
    executable: &Path,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    cancellation: &CancellationToken,
) -> Result<ProviderCapabilities, String> {
    let launch =
        prepare_provider_launch(executable, ["app-server"]).map_err(|error| error.to_string())?;
    let mut command = Command::new(launch.program);
    command
        .args(launch.args)
        .current_dir(cwd)
        .envs(environment.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sanitize_provider_subprocess_environment(&mut command);
    let mut child = supervised_command(command)
        .spawn()
        .map_err(|error| error.to_string())?;
    let result = async {
        let stdout = child.stdout().take().ok_or("Codex stdout is unavailable")?;
        let stdin = child.stdin().take().ok_or("Codex stdin is unavailable")?;
        let stderr = child.stderr().take().ok_or("Codex stderr is unavailable")?;
        let (connection, _incoming) = codex::JsonRpcConnection::spawn(stdout, stdin, stderr, codex::ConnectionConfig::default());
        let cwd = cwd.to_string_lossy();

        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err("Skill discovery was cancelled.".to_owned()),
            result = timeout(PROBE_TIMEOUT, read_codex_capabilities(&connection, &cwd)) => result.unwrap_or_else(|_| Err("Codex skill discovery timed out. Retry to reload the catalog.".to_owned())),
        };
        connection.close().await;
        result
    }.await;
    stop_supervised_child(&mut *child).await;
    result
}

async fn read_codex_capabilities(
    connection: &codex::JsonRpcConnection,
    cwd: &str,
) -> Result<ProviderCapabilities, String> {
    connection
        .request(
            "initialize",
            codex::build_initialize_params(env!("CARGO_PKG_VERSION")),
        )
        .await
        .map_err(|error| error.to_string())?;
    connection
        .notify_without_params("initialized")
        .await
        .map_err(|error| error.to_string())?;
    let response = connection
        .request("skills/list", json!({"cwds":[cwd],"forceReload":true}))
        .await
        .map_err(|error| error.to_string())?;
    let entry = response
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| {
            data.iter()
                .find(|entry| entry["cwd"] == cwd)
                .or_else(|| (data.len() == 1).then(|| &data[0]))
        })
        .filter(|entry| entry["skills"].is_array())
        .ok_or("Codex returned an incomplete skill catalog")?;
    let issues = entry
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .unwrap_or("Codex could not load a skill. Check its SKILL.md and retry.")
                .to_owned()
        })
        .collect();
    let skills = codex::parse_skills_list_response(&response, cwd)?;
    Ok(ProviderCapabilities {
        skills: codex_skill_inventory(&skills),
        issues,
        ..ProviderCapabilities::default()
    })
}

/// `protocol` is known for a server BiBCode started; a configured Server URL is identified first.
async fn opencode_capabilities(
    endpoint: &str,
    password: Option<&str>,
    cwd: &Path,
    protocol: Option<opencode::OpenCodeProtocol>,
) -> Option<ProviderCapabilities> {
    let protocol = match protocol {
        Some(protocol) => protocol,
        None => opencode_server_protocol(endpoint, password).await.ok()?,
    };
    let client = reqwest::Client::builder()
        .timeout(LOCAL_OPENCODE_INVENTORY_TIMEOUT)
        .build()
        .ok()?;
    let endpoint = endpoint.trim_end_matches('/');
    let (commands, agents) = match protocol {
        opencode::OpenCodeProtocol::V1 => {
            let (commands, agents) = tokio::join!(
                get_opencode_json(&client, endpoint, "/command", password, Some(cwd)),
                get_opencode_json(&client, endpoint, "/agent", password, Some(cwd)),
            );
            (commands?, agents?)
        }
        opencode::OpenCodeProtocol::V2 => {
            // Only the slash-menu catalogs: a failing model list must not hide skills.
            let [agents, commands, skills] = read_opencode_v2_catalogs(
                &client,
                endpoint,
                password,
                Some(cwd),
                ["agent", "command", "skill"],
            )
            .await?;
            use opencode::v2::catalog;
            (
                catalog::commands(&commands, &skills),
                catalog::agents(&agents),
            )
        }
    };
    if !commands.is_array() || !agents.is_array() {
        return None;
    }
    let snapshot = opencode::build_inventory_snapshot(&json!({}), &agents, &commands, &[]);
    Some(ProviderCapabilities {
        slash_commands: snapshot.commands,
        skills: snapshot.skills,
        agents: snapshot.agents,
        ..ProviderCapabilities::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::test_support::TestSandbox;

    #[cfg(unix)]
    #[tokio::test]
    async fn native_skill_catalogs_use_user_configuration_and_the_selected_workspace() {
        let sandbox = TestSandbox::new("native-skill-catalogs");
        let home = sandbox.path("user");
        let shared = sandbox.path("codex-shared");
        let shadow = sandbox.path("codex-shadow");
        let claude = sandbox.path("claude-config");
        for root in [&shared, &claude] {
            std::fs::create_dir_all(root.join("skills/personal")).unwrap();
            std::fs::write(root.join("skills/personal/SKILL.md"), "# Personal").unwrap();
        }
        std::fs::create_dir_all(&home).unwrap();
        let binary = sandbox.executable_script("catalog", r#"
while IFS= read -r request; do
  case "$request" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}' ;;
    *'"method":"skills/list"'*)
      test -f "$CODEX_HOME/skills/personal/SKILL.md" || exit 9
      case "$request" in *"$PWD"*) ;; *) exit 10 ;; esac
      printf '{"id":2,"result":%s}\n' "$(cat codex-catalog.json)" ;;
    *'"subtype":"initialize"'*) printf '{"type":"control_response","response":{"subtype":"success","request_id":"bibcode-inventory","response":%s}}\n' "$(cat claude-commands.json)" ;;
    *'"subtype":"reload_skills"'*)
      test -f "$CLAUDE_CONFIG_DIR/skills/personal/SKILL.md" || exit 11
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"bibcode-skills","response":%s}}\n' "$(cat claude-catalog.json)" ;;
    *'"method":"initialized"'*) ;;
    *) exit 12 ;;
  esac
done
"#, "");
        for (driver, instance) in [
            ("codex", "codex-personal"),
            ("claudeAgent", "claude-personal"),
        ] {
            let settings = json!({"providerInstances": {instance: {
                "driver":driver,"enabled":true,
                "config":{"binaryPath":binary,"homePath":shared,"shadowHomePath":shadow},
                "environment":[{"name":"HOME","value":home},{"name":"CLAUDE_CONFIG_DIR","value":claude}]
            }}});
            for project_name in ["empty", "first", "second"] {
                let cwd = sandbox.path(project_name);
                std::fs::create_dir_all(&cwd).unwrap();
                // The child shell reports physical PWD; use that same fixture
                // spelling while still asserting the exact requested workspace.
                let cwd = std::fs::canonicalize(cwd).unwrap();
                let mut skills = vec![
                    json!({"name":"personal","path":shared.join("skills/personal/SKILL.md"),"enabled":true,"scope":"user"}),
                ];
                if project_name != "empty" {
                    skills.push(json!({"name":project_name,"path":cwd.join("SKILL.md"),"enabled":true,"scope":"repo"}));
                }
                std::fs::write(
                    cwd.join("codex-catalog.json"),
                    json!({"data":[{"cwd":cwd,"skills":skills,"errors":[{"message":"A separate skill could not load."}]}]}).to_string(),
                )
                .unwrap();
                let mut commands = skills
                    .iter()
                    .map(|skill| json!({"name":skill["name"]}))
                    .collect::<Vec<_>>();
                commands[0] = json!({"name":"personal","aliases":["plugin:personal"]});
                commands.push(json!({"name":"manual-only"}));
                std::fs::write(
                    cwd.join("claude-commands.json"),
                    json!({"commands":commands,"agents":[]}).to_string(),
                )
                .unwrap();
                skills[0]["name"] = json!("plugin:personal");
                skills.push(json!({"name":"hidden-model-skill"}));
                std::fs::write(
                    cwd.join("claude-catalog.json"),
                    json!({"skills":skills}).to_string(),
                )
                .unwrap();
                let catalog = super::super::discover_capabilities(
                    &settings,
                    instance,
                    &cwd,
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
                assert_eq!(
                    catalog["skills"].as_array().unwrap().len(),
                    if project_name == "empty" { 1 } else { 2 }
                );
                assert_eq!(catalog["skills"][0]["name"], "personal");
                if driver == "codex" {
                    assert_eq!(
                        catalog["issues"],
                        json!(["A separate skill could not load."])
                    );
                }
                if driver == "claudeAgent" {
                    assert!(
                        catalog["slashCommands"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|command| command["name"] == "manual-only")
                    );
                    assert!(
                        catalog["skills"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|skill| skill["name"] != "hidden-model-skill")
                    );
                    assert_eq!(
                        catalog["skills"][0]["path"],
                        "claude://skill/plugin:personal"
                    );
                }
                assert_eq!(
                    catalog["skills"][0]["invocation"],
                    if driver == "codex" { "dollar" } else { "slash" }
                );
            }
        }
    }

    #[tokio::test]
    async fn opencode_skill_catalogs_use_the_remote_workspace_directory() {
        use axum::{Json, Router, extract::Query, http::HeaderMap, routing::get};
        use std::collections::HashMap;
        let app = Router::new()
            .route(
                "/command",
                get(
                    |Query(query): Query<HashMap<String, String>>, headers: HeaderMap| async move {
                        assert_eq!(headers["authorization"], "Basic b3BlbmNvZGU6c2VjcmV0");
                        let cwd = query.get("directory").expect("workspace directory");
                        let name = Path::new(cwd).file_name().unwrap().to_str().unwrap();
                        let mut commands = vec![
                            json!({"name":"personal","source":"skill"}),
                            json!({"name":"review","source":"command"}),
                        ];
                        if name != "empty" {
                            commands.push(json!({"name":name,"source":"skill"}));
                        }
                        Json(json!(commands))
                    },
                ),
            )
            .route(
                "/agent",
                get(|Query(query): Query<HashMap<String, String>>| async move {
                    assert!(query.contains_key("directory"));
                    Json(json!([]))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let workspace = tempfile::tempdir().unwrap();
        let settings = json!({"providerInstances":{"remote":{"driver":"opencode","config":{"serverUrl":endpoint,"serverPassword":"secret"}}}});
        for project in ["empty", "first & #", "second"] {
            let cwd = workspace.path().join(project);
            tokio::fs::create_dir_all(&cwd).await.unwrap();
            let result = super::super::discover_capabilities(
                &settings,
                "remote",
                &cwd,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(result["skills"][0]["name"], "personal");
            assert_eq!(
                result["skills"].as_array().unwrap().len(),
                if project == "empty" { 1 } else { 2 }
            );
            if project != "empty" {
                assert_eq!(result["skills"][1]["name"], project);
            }
            assert_eq!(result["slashCommands"], json!([{"name":"review"}]));
        }
        server.abort();
        let _ = server.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_opencode_skill_catalog_uses_the_configured_home_and_workspace() {
        const FIXTURE_PORT: &str = "BIBCODE_TEST_SKILLS_HTTP_PORT";
        if let Ok(port) = std::env::var(FIXTURE_PORT) {
            use axum::{Json, Router, extract::Query, http::HeaderMap, routing::get};
            use base64::Engine as _;
            let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
            assert!(home.join(".agents/skills/personal/SKILL.md").is_file());
            let app = Router::new()
                .route("/global/health", get(|| async { Json(json!({"healthy":true})) }))
                .route("/command", get(|Query(query): Query<HashMap<String, String>>, headers: HeaderMap| async move {
                    let cwd = std::env::current_dir().unwrap();
                    assert_eq!(query["directory"], cwd.to_string_lossy());
                    let password = std::env::var("OPENCODE_SERVER_PASSWORD").unwrap();
                    assert_eq!(headers["authorization"], format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))));
                    let name = cwd.file_name().unwrap().to_str().unwrap();
                    let mut commands = vec![json!({"name":"personal","source":"skill"})];
                    if name != "empty" {
                        commands.push(json!({"name":name,"source":"skill"}));
                    }
                    Json(json!(commands))
                }))
                .route("/agent", get(|Query(query): Query<HashMap<String, String>>| async move {
                    assert_eq!(query["directory"], std::env::current_dir().unwrap().to_string_lossy());
                    Json(json!([]))
                }));
            let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
                .await
                .unwrap();
            axum::serve(listener, app).await.unwrap();
            return;
        }
        let sandbox = TestSandbox::new("local-opencode-skills");
        let skill = sandbox.path("home/.agents/skills/personal");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# Personal").unwrap();
        let binary = sandbox.executable_script("opencode", r#"
for argument in "$@"; do
  case "$argument" in --port=*) export BIBCODE_TEST_SKILLS_HTTP_PORT="${argument#--port=}" ;; esac
done
exec "$BIBCODE_TEST_EXECUTABLE" --exact production::provider_inventory::capabilities::tests::local_opencode_skill_catalog_uses_the_configured_home_and_workspace --nocapture
"#, "");
        let settings = json!({"providerInstances":{"local":{"driver":"opencode","config":{"binaryPath":binary},"environment":[
            {"name":"HOME","value":sandbox.path("home")},
            {"name":"BIBCODE_TEST_EXECUTABLE","value":std::env::current_exe().unwrap()}
        ]}}});
        for project in ["empty", "first & #", "second"] {
            let cwd = sandbox.path(project);
            std::fs::create_dir_all(&cwd).unwrap();
            // The local HTTP fixture compares against its process current_dir.
            let cwd = std::fs::canonicalize(cwd).unwrap();
            let result = discover_capabilities(&settings, "local", &cwd, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(result["skills"][0]["name"], "personal");
            assert_eq!(
                result["skills"].as_array().unwrap().len(),
                if project == "empty" { 1 } else { 2 }
            );
            if project != "empty" {
                assert_eq!(result["skills"][1]["name"], project);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_skill_discovery_reaps_each_native_probe() {
        for driver in ["codex", "claudeAgent", "opencode"] {
            let sandbox = TestSandbox::new("cancelled-skills");
            let pid_file = sandbox.path("pid");
            let binary = sandbox.executable_script(
                "slow-provider",
                "printf '%s' \"$$\" > \"$BIBCODE_TEST_PID\"\nexec sleep 60",
                "",
            );
            let settings = json!({"providerInstances":{"probe":{"driver":driver,"config":{"binaryPath":binary},"environment":[{"name":"HOME","value":sandbox.root()},{"name":"BIBCODE_TEST_PID","value":pid_file}]}}});
            let cancellation = CancellationToken::new();
            let task = tokio::spawn({
                let cancellation = cancellation.clone();
                let cwd = sandbox.root().to_path_buf();
                async move {
                    super::super::discover_capabilities(&settings, "probe", &cwd, &cancellation)
                        .await
                }
            });
            let pid: libc::pid_t = timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(raw) = tokio::fs::read_to_string(&pid_file).await
                        && let Ok(pid) = raw.parse()
                    {
                        break pid;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("native probe started");
            cancellation.cancel();
            assert!(
                timeout(Duration::from_secs(5), task)
                    .await
                    .expect("probe stopped after cancellation")
                    .unwrap()
                    .is_err()
            );
            // SAFETY: signal 0 only checks the liveness of the fixture PID.
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "{driver} discovery child was not reaped"
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }

    /// The slash menu of an OpenCode 2 server loads from its agent, command and skill lists alone,
    /// so a failing model list does not hide skills.
    #[tokio::test]
    async fn opencode_2_slash_catalogs_load_when_the_model_list_fails() {
        use axum::{Json, http::StatusCode, routing::get};
        let path = format!(
            "{}/tests/fixtures/opencode-v2/catalog.exchanges.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let exchanges: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("catalog fixture"))
                .expect("catalog JSON");
        let captured = |route: &str| {
            exchanges["exchanges"]
                .as_array()
                .unwrap()
                .iter()
                .find(|exchange| {
                    exchange["method"] == "GET"
                        && exchange["path"].as_str().unwrap().split('?').next() == Some(route)
                })
                .expect("captured catalog")["response"]
                .clone()
        };
        let mut app = opencode::server_fixtures::v2("secret").route(
            "/api/model",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        );
        for route in ["/api/agent", "/api/command", "/api/skill"] {
            let response = captured(route);
            app = app.route(
                route,
                get(move || {
                    let response = response.clone();
                    async move { Json(response) }
                }),
            );
        }
        let (endpoint, server) = opencode::server_fixtures::serve(app).await;

        let capabilities =
            opencode_capabilities(&endpoint, Some("secret"), Path::new("/work/project"), None)
                .await;
        server.abort();

        let capabilities = capabilities.expect("the slash menu loads");
        assert!(
            capabilities
                .skills
                .iter()
                .any(|skill| skill["name"] == "report")
        );
        assert!(
            capabilities
                .slash_commands
                .iter()
                .any(|command| command["name"] == "review")
        );
    }
}
