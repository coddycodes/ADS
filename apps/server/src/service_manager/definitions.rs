use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) const SYSTEMD_UNIT_NAME: &str = "coddy-studio.service";
pub(crate) const LAUNCHD_LABEL: &str = "com.coddy.studio.server";
pub(crate) const WINDOWS_TASK_NAME: &str = "Coddy Studio Server";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceSpec {
    pub executable: PathBuf,
    pub host: String,
    pub port: u16,
    pub base_dir: Option<PathBuf>,
    pub static_dir: Option<PathBuf>,
    pub path_env: Option<OsString>,
}

impl ServiceSpec {
    /// Arguments after the executable. Only the flags a service needs are
    /// included; `--mode` and `--bootstrap-fd` are desktop-host concerns.
    pub(crate) fn serve_arguments(&self) -> Vec<OsString> {
        let mut arguments: Vec<OsString> = vec![
            "serve".into(),
            "--host".into(),
            self.host.clone().into(),
            "--port".into(),
            self.port.to_string().into(),
        ];
        if let Some(base_dir) = &self.base_dir {
            arguments.push("--base-dir".into());
            arguments.push(base_dir.clone().into_os_string());
        }
        if let Some(static_dir) = &self.static_dir {
            arguments.push("--static-dir".into());
            arguments.push(static_dir.clone().into_os_string());
        }
        arguments.push("--no-startup-pairing-offer".into());
        arguments
    }
}

/// systemd quoting: double-quote every word that is a path (may contain
/// spaces), escaping backslashes and double quotes.
fn systemd_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

fn is_path_word(argument: &OsString, previous: Option<&OsString>) -> bool {
    previous.is_some_and(|flag| flag == "--base-dir" || flag == "--static-dir")
        || argument.to_string_lossy().contains(' ')
}

pub(crate) fn render_systemd_unit(spec: &ServiceSpec) -> String {
    let mut exec_start = systemd_quote(&spec.executable.to_string_lossy());
    let arguments = spec.serve_arguments();
    for (index, argument) in arguments.iter().enumerate() {
        exec_start.push(' ');
        let text = argument.to_string_lossy();
        if is_path_word(
            argument,
            index
                .checked_sub(1)
                .and_then(|previous| arguments.get(previous)),
        ) {
            exec_start.push_str(&systemd_quote(&text));
        } else {
            exec_start.push_str(&text);
        }
    }
    let mut unit = String::new();
    unit.push_str("[Unit]\n");
    unit.push_str("Description=Coddy Studio server\n");
    unit.push_str("After=network-online.target\n");
    unit.push_str("Wants=network-online.target\n\n");
    unit.push_str("[Service]\n");
    unit.push_str(&format!("ExecStart={exec_start}\n"));
    if let Some(path) = &spec.path_env {
        unit.push_str(&format!(
            "Environment={}\n",
            systemd_quote(&format!("PATH={}", path.to_string_lossy()))
        ));
    }
    unit.push_str("Restart=on-failure\n");
    unit.push_str("RestartSec=2\n\n");
    unit.push_str("[Install]\n");
    unit.push_str("WantedBy=default.target\n");
    unit
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            other => escaped.push(other),
        }
    }
    escaped
}

pub(crate) fn render_launchd_plist(spec: &ServiceSpec, log_path: &Path) -> String {
    let mut plist = String::new();
    plist.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    plist.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    plist.push_str("<plist version=\"1.0\">\n<dict>\n");
    plist.push_str(&format!(
        "\t<key>Label</key>\n\t<string>{LAUNCHD_LABEL}</string>\n"
    ));
    plist.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    plist.push_str(&format!(
        "\t\t<string>{}</string>\n",
        xml_escape(&spec.executable.to_string_lossy())
    ));
    for argument in spec.serve_arguments() {
        plist.push_str(&format!(
            "\t\t<string>{}</string>\n",
            xml_escape(&argument.to_string_lossy())
        ));
    }
    plist.push_str("\t</array>\n");
    if let Some(path) = &spec.path_env {
        plist.push_str("\t<key>EnvironmentVariables</key>\n\t<dict>\n");
        plist.push_str(&format!(
            "\t\t<key>PATH</key>\n\t\t<string>{}</string>\n",
            xml_escape(&path.to_string_lossy())
        ));
        plist.push_str("\t</dict>\n");
    }
    plist.push_str("\t<key>RunAtLoad</key>\n\t<true/>\n");
    plist.push_str("\t<key>KeepAlive</key>\n\t<true/>\n");
    let log = xml_escape(&log_path.to_string_lossy());
    plist.push_str(&format!(
        "\t<key>StandardOutPath</key>\n\t<string>{log}</string>\n"
    ));
    plist.push_str(&format!(
        "\t<key>StandardErrorPath</key>\n\t<string>{log}</string>\n"
    ));
    plist.push_str("</dict>\n</plist>\n");
    plist
}

fn windows_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

/// The `/TR` value for `schtasks`: the quoted executable followed by the
/// serve arguments, quoting path words.
pub(crate) fn windows_task_command(spec: &ServiceSpec) -> String {
    let mut command = windows_quote(&spec.executable.to_string_lossy());
    let arguments = spec.serve_arguments();
    for (index, argument) in arguments.iter().enumerate() {
        command.push(' ');
        let text = argument.to_string_lossy();
        if is_path_word(
            argument,
            index
                .checked_sub(1)
                .and_then(|previous| arguments.get(previous)),
        ) {
            command.push_str(&windows_quote(&text));
        } else {
            command.push_str(&text);
        }
    }
    command
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn spec() -> ServiceSpec {
        ServiceSpec {
            executable: PathBuf::from("/usr/bin/coddy-studio"),
            host: "100.105.196.60".to_owned(),
            port: 3773,
            base_dir: Some(PathBuf::from("/home/me/.coddy/studio")),
            static_dir: None,
            path_env: Some("/home/me/.local/bin:/usr/bin".into()),
        }
    }

    #[test]
    fn serve_arguments_carry_only_the_service_flags() {
        let arguments = spec().serve_arguments();
        assert_eq!(
            arguments,
            vec![
                "serve",
                "--host",
                "100.105.196.60",
                "--port",
                "3773",
                "--base-dir",
                "/home/me/.coddy/studio",
                "--no-startup-pairing-offer",
            ]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
        );
        let mut with_static = spec();
        with_static.base_dir = None;
        with_static.static_dir = Some(PathBuf::from("/opt/coddy-studio/web"));
        let arguments = with_static.serve_arguments();
        assert!(arguments.iter().any(|argument| argument == "--static-dir"));
        assert!(!arguments.iter().any(|argument| argument == "--base-dir"));
    }

    #[test]
    fn renders_a_systemd_user_unit_with_quoted_exec_and_captured_path() {
        assert_eq!(SYSTEMD_UNIT_NAME, "coddy-studio.service");
        let unit = render_systemd_unit(&spec());
        assert_eq!(
            unit,
            "[Unit]\n\
             Description=Coddy Studio server\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             ExecStart=\"/usr/bin/coddy-studio\" serve --host 100.105.196.60 --port 3773 --base-dir \"/home/me/.coddy/studio\" --no-startup-pairing-offer\n\
             Environment=\"PATH=/home/me/.local/bin:/usr/bin\"\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );
    }

    #[test]
    fn systemd_quoting_escapes_backslashes_and_quotes() {
        let mut spec = spec();
        spec.executable = PathBuf::from("/opt/my \"apps\"/bib\\code");
        spec.path_env = None;
        let unit = render_systemd_unit(&spec);
        assert!(
            unit.contains("ExecStart=\"/opt/my \\\"apps\\\"/bib\\\\code\" serve"),
            "{unit}"
        );
        assert!(!unit.contains("Environment="), "{unit}");
    }

    #[test]
    fn renders_a_launch_agent_plist_with_escaped_values() {
        assert_eq!(LAUNCHD_LABEL, "com.coddy.studio.server");
        let mut spec = spec();
        spec.executable = PathBuf::from("/Applications/Bib&Code/coddy-studio");
        let plist = render_launchd_plist(
            &spec,
            Path::new("/Users/me/Library/Logs/coddy-studio-server.log"),
        );
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(
            plist.contains("<key>Label</key>\n\t<string>com.coddy.studio.server</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/Applications/Bib&amp;Code/coddy-studio</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>--no-startup-pairing-offer</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<key>PATH</key>\n\t\t<string>/home/me/.local/bin:/usr/bin</string>"),
            "{plist}"
        );
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"), "{plist}");
        assert!(plist.contains("<key>KeepAlive</key>\n\t<true/>"), "{plist}");
        assert!(
            plist.contains(
                "<key>StandardOutPath</key>\n\t<string>/Users/me/Library/Logs/coddy-studio-server.log</string>"
            ),
            "{plist}"
        );
    }

    #[test]
    fn windows_task_command_quotes_the_executable_and_paths() {
        assert_eq!(WINDOWS_TASK_NAME, "Coddy Studio Server");
        let mut spec = spec();
        spec.executable = PathBuf::from(r"C:\Program Files\Coddy Studio\coddy-studio.exe");
        spec.base_dir = Some(PathBuf::from(r"C:\Users\me\.coddy/studio"));
        assert_eq!(
            windows_task_command(&spec),
            r#""C:\Program Files\Coddy Studio\coddy-studio.exe" serve --host 100.105.196.60 --port 3773 --base-dir "C:\Users\me\.coddy/studio" --no-startup-pairing-offer"#
        );
    }
}
