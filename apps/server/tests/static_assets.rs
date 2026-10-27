use std::path::Path;

use coddy_studio_server::remote_update::RemoteUpdateInstallKind;
use coddy_studio_server::{StaticDirError, StaticDirSource, resolve_static_dir};

#[test]
fn packaged_web_is_resolved_beside_the_executable() {
    let root = tempfile::tempdir().expect("distribution root");
    let executable = root.path().join("coddy-studio");
    std::fs::write(&executable, b"binary").expect("binary fixture");
    std::fs::create_dir(root.path().join("web")).expect("web directory");
    std::fs::write(
        root.path().join("web/index.html"),
        b"<main>Coddy Studio</main>",
    )
    .expect("web entry point");

    let resolved = resolve_static_dir(None, &executable)
        .expect("resolve packaged web")
        .expect("packaged static directory");

    assert_eq!(resolved.source, StaticDirSource::Packaged);
    assert_eq!(resolved.path, root.path().join("web"));
    assert_eq!(resolved.install_kind, RemoteUpdateInstallKind::Archive);
}

#[test]
fn installed_web_is_resolved_from_the_executable_prefix() {
    let root = tempfile::tempdir().expect("installation prefix");
    let executable = root.path().join("bin/coddy-studio");
    let web = root.path().join("share/coddy-studio/web");
    std::fs::create_dir_all(executable.parent().expect("bin parent")).expect("bin directory");
    std::fs::create_dir_all(&web).expect("installed web directory");
    std::fs::write(&executable, b"binary").expect("binary fixture");
    std::fs::write(web.join("index.html"), b"<main>Installed</main>")
        .expect("installed web entry point");

    let resolved = resolve_static_dir(None, &executable)
        .expect("resolve installed web")
        .expect("installed static directory");

    assert_eq!(resolved.source, StaticDirSource::Packaged);
    assert_eq!(resolved.path, web);
    assert_eq!(
        resolved.install_kind,
        RemoteUpdateInstallKind::SystemPackage
    );
}

#[test]
fn invalid_explicit_web_does_not_fall_back_to_packaged_assets() {
    let root = tempfile::tempdir().expect("distribution root");
    let executable = root.path().join("coddy-studio");
    std::fs::write(&executable, b"binary").expect("binary fixture");
    std::fs::create_dir(root.path().join("web")).expect("web directory");
    std::fs::write(root.path().join("web/index.html"), b"packaged").expect("packaged entry point");
    let missing = root.path().join("missing");

    let error = resolve_static_dir(Some(&missing), &executable)
        .expect_err("invalid explicit static directory");

    assert!(matches!(error, StaticDirError::ExplicitInvalid { path } if path == missing));
}

#[test]
fn explicit_web_wins_over_packaged_assets() {
    let root = tempfile::tempdir().expect("distribution root");
    let executable = root.path().join("coddy-studio");
    let explicit = root.path().join("explicit");
    for directory in [root.path().join("web"), explicit.clone()] {
        std::fs::create_dir(&directory).expect("static directory");
        std::fs::write(directory.join("index.html"), b"entry").expect("static entry point");
    }
    std::fs::write(&executable, b"binary").expect("binary fixture");

    let resolved = resolve_static_dir(Some(Path::new(&explicit)), &executable)
        .expect("resolve explicit web")
        .expect("explicit static directory");

    assert_eq!(resolved.source, StaticDirSource::Explicit);
    assert_eq!(resolved.path, explicit);
    assert_eq!(resolved.install_kind, RemoteUpdateInstallKind::Unknown);
}
