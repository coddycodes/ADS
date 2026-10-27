use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
};

use super::reexec;

const CASE: &str = "CODDY_STUDIO_APPIMAGE_ENVIRONMENT_TEST_CASE";
const APPDIR_FIXTURE: &str = "CODDY_STUDIO_TEST_APPIMAGE_ROOT";
pub const HOST_PATH_FIXTURE: &str = "CODDY_STUDIO_TEST_ORIGINAL_HOST_PATH";
const PATH_VARIABLES: &[&str] = &[
    "LD_LIBRARY_PATH",
    "PATH",
    "XDG_DATA_DIRS",
    "GTK_PATH",
    "GIO_MODULE_DIR",
    "GSETTINGS_SCHEMA_DIR",
    "PERLLIB",
    "GDK_PIXBUF_MODULE_FILE",
    "GDK_PIXBUF_MODULEDIR",
    "LD_PRELOAD",
    "GIO_EXTRA_MODULES",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "PYTHONHOME",
    "PYTHONPATH",
    "QT_PLUGIN_PATH",
    "CODDY_STUDIO_TEST_FUTURE_PLUGIN_PATH",
];

pub const ENVIRONMENT_CASES: &[&str] = &[
    "mixed",
    "bundled-only",
    "launcher-empty-original",
    "launcher-host-original",
    "no-library-path",
    "unset-appimage",
    "empty-appimage",
    "relative-appdir",
    "root-appdir",
    "unset-appdir",
];

pub type Environment = BTreeMap<&'static str, Option<OsString>>;

fn fixture(case: &str, appdir: &str) -> (Environment, Environment) {
    let bundled = format!("{appdir}/usr/lib:/tmp/.mount_bibcodOLD/usr/lib");
    let host = format!("/opt/host first:{appdir}-host/lib:/usr/bin:/bin:.mount_relative/lib");
    let mixed = format!("{appdir}/usr/lib:{host}:/tmp/.mount_bibcodOLD/usr/lib");
    let mut original = Environment::from([
        ("APPIMAGE", Some("/opt/coddy-studio.AppImage".into())),
        ("APPDIR", Some(appdir.into())),
        ("ARGV0", Some("coddy-studio.AppImage".into())),
        ("OWD", Some("/opt/launch-directory".into())),
        ("GTK_THEME", Some("Adwaita:dark".into())),
        ("GDK_BACKEND", Some("wayland,x11".into())),
        ("PYTHONDONTWRITEBYTECODE", Some("1".into())),
        ("SSH_AUTH_SOCK", Some("/tmp/host-agent.sock".into())),
        ("LS_COLORS", Some("di=01;34:*.rs=01;32:ln=target".into())),
        (
            "CODDY_STUDIO_TEST_UNRELATED_BYTES",
            Some(OsString::from_vec(b"raw:\xff::keep".to_vec())),
        ),
        (
            "CODDY_STUDIO_TEST_RAW_PLUGIN_PATH",
            Some(OsString::from_vec(
                [format!("{appdir}/usr/lib:").as_bytes(), b"/host/\xff"].concat(),
            )),
        ),
    ]);
    let mut expected = Environment::from([
        ("APPIMAGE", None),
        ("APPDIR", None),
        ("ARGV0", None),
        ("OWD", None),
        ("GTK_THEME", None),
        ("GDK_BACKEND", None),
        ("PYTHONDONTWRITEBYTECODE", None),
        ("SSH_AUTH_SOCK", Some("/tmp/host-agent.sock".into())),
        ("LS_COLORS", Some("di=01;34:*.rs=01;32:ln=target".into())),
        (
            "CODDY_STUDIO_TEST_UNRELATED_BYTES",
            Some(OsString::from_vec(b"raw:\xff::keep".to_vec())),
        ),
        (
            "CODDY_STUDIO_TEST_RAW_PLUGIN_PATH",
            Some(OsString::from_vec(b"/host/\xff".to_vec())),
        ),
    ]);
    for &name in PATH_VARIABLES {
        let (value, retained) = if name == "GTK_PATH" {
            (
                format!(
                    "{appdir}//usr/lib/x86_64-linux-gnu/gtk-3.0:/usr/lib64/gtk-3.0:/usr/lib/x86_64-linux-gnu/gtk-3.0"
                ),
                None,
            )
        } else if name == "PYTHONHOME" {
            (format!("{appdir}/usr/"), None)
        } else if case == "launcher-empty-original" {
            (format!("{bundled}:"), None)
        } else if case == "launcher-host-original" && name == "LD_LIBRARY_PATH" {
            // Preserve the original host list, including empties beside real entries.
            (
                format!("{bundled}:/host/first::/host/second:"),
                Some("/host/first::/host/second:".into()),
            )
        } else if case == "bundled-only" {
            (bundled.clone(), None)
        } else if name == "CODDY_STUDIO_TEST_FUTURE_PLUGIN_PATH" {
            (
                format!("{appdir}/usr/lib/x:/host/keep"),
                Some("/host/keep".into()),
            )
        } else if name == "LD_PRELOAD" {
            // The loader accepts spaces as well as colons for preloads.
            (
                format!("{appdir}/preload.so libc.so.6:/tmp/.mount_old/preload.so libm.so.6"),
                Some("libc.so.6:libm.so.6".into()),
            )
        } else if name == "LD_LIBRARY_PATH" {
            (
                format!("{appdir}/usr/lib;{host}:/tmp/.mount_bibcodOLD/usr/lib"),
                Some(host.clone().into()),
            )
        } else {
            (mixed.clone(), Some(host.clone().into()))
        };
        original.insert(name, Some(value.into()));
        expected.insert(name, retained);
    }
    match case {
        "mixed" | "bundled-only" | "launcher-empty-original" | "launcher-host-original" => {}
        "no-library-path" => {
            original.insert("LD_LIBRARY_PATH", None);
            expected.insert("LD_LIBRARY_PATH", None);
        }
        "unset-appimage" => {
            original.insert("APPIMAGE", None);
            expected = original.clone();
        }
        "empty-appimage" => {
            original.insert("APPIMAGE", Some("".into()));
            expected = original.clone();
        }
        "relative-appdir" | "root-appdir" | "unset-appdir" => {
            original.insert(
                "APPDIR",
                match case {
                    "relative-appdir" => Some("relative/AppDir".into()),
                    "root-appdir" => Some("/".into()),
                    _ => None,
                },
            );
            expected = original.clone();
        }
        _ => panic!("unknown AppImage fixture case: {case}"),
    }
    (original, expected)
}

// Re-exec rather than mutate global environment in a parallel test harness.
// Every seam checks the same inherited inputs and the complete parent snapshot.
pub fn check_inherited_environment(
    test_name: &str,
    cases: &[&str],
    check: impl FnOnce(&Environment),
) {
    const PHASE: &str = "appimage-environment";
    if let Some(child) = reexec::enter(test_name, PHASE) {
        let case = std::env::var(CASE).expect("AppImage fixture case");
        let parent = std::env::vars_os().collect::<BTreeMap<_, _>>();
        let (_, expected) = fixture(&case, &std::env::var(APPDIR_FIXTURE).unwrap());
        check(&expected);
        assert_eq!(std::env::vars_os().collect::<BTreeMap<_, _>>(), parent);
        child.complete();
        return;
    }
    let directory = tempfile::tempdir().expect("AppImage fixture directory");
    let appdir = directory.path().join("AppDir");
    std::fs::create_dir_all(appdir.join("usr")).expect("empty bundled Python home");
    for case in cases {
        reexec::run(test_name, PHASE, None, |command| {
            command.env(CASE, case);
            command.env(APPDIR_FIXTURE, &appdir);
            command.env(
                HOST_PATH_FIXTURE,
                std::env::var_os("PATH").unwrap_or_default(),
            );
            for (name, value) in fixture(case, appdir.to_str().unwrap()).0 {
                match value {
                    Some(value) => command.env(name, value),
                    None => command.env_remove(name),
                };
            }
        });
    }
}

pub fn assert_child_environment(output: &[u8], expected: &Environment) {
    let environment: BTreeMap<_, _> = output
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let separator = entry.iter().position(|byte| *byte == b'=')?;
            Some((
                OsStr::from_bytes(&entry[..separator]),
                OsStr::from_bytes(&entry[separator + 1..]),
            ))
        })
        .collect();
    for (name, value) in expected {
        assert_eq!(
            environment.get(OsStr::new(name)).copied(),
            value.as_deref(),
            "child environment variable {name}"
        );
    }
}
