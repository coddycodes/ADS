use base64::{Engine as _, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fmt, fs,
    path::Path,
};
use url::Url;

const UPDATE_TARGETS: [&str; 6] = [
    "darwin-aarch64",
    "darwin-x86_64",
    "linux-aarch64",
    "linux-x86_64",
    "windows-aarch64",
    "windows-x86_64",
];

#[derive(Debug)]
struct VerificationError(String);

impl fmt::Display for VerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for VerificationError {}

#[derive(Deserialize)]
struct TauriConfig {
    plugins: TauriPlugins,
}

#[derive(Deserialize)]
struct TauriPlugins {
    updater: UpdaterConfig,
}

#[derive(Deserialize)]
struct UpdaterConfig {
    pubkey: String,
}

#[derive(Deserialize)]
struct UpdateManifest {
    platforms: BTreeMap<String, UpdatePlatform>,
}

#[derive(Deserialize)]
struct UpdatePlatform {
    signature: String,
    url: String,
}

fn verification_error(message: impl Into<String>) -> VerificationError {
    VerificationError(message.into())
}

fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    description: &str,
) -> Result<T, VerificationError> {
    let bytes = fs::read(path)
        .map_err(|error| verification_error(format!("Could not read {description}: {error}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| verification_error(format!("Invalid {description}: {error}")))
}

fn decode_outer_base64(encoded: &str, description: &str) -> Result<String, VerificationError> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| verification_error(format!("Invalid base64 in {description}")))?;
    String::from_utf8(bytes)
        .map_err(|_| verification_error(format!("Invalid UTF-8 in {description}")))
}

fn artifact_name(url: &str, target: &str) -> Result<String, VerificationError> {
    let url = Url::parse(url)
        .map_err(|_| verification_error(format!("Invalid updater URL for {target}")))?;
    let encoded_name = url
        .path_segments()
        .and_then(Iterator::last)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| verification_error(format!("Missing updater artifact name for {target}")))?;
    let name = percent_decode_str(encoded_name)
        .decode_utf8()
        .map_err(|_| verification_error(format!("Invalid updater artifact name for {target}")))?
        .into_owned();
    if name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || Path::new(&name)
            .file_name()
            .and_then(|value| value.to_str())
            != Some(name.as_str())
    {
        return Err(verification_error(format!(
            "Unsafe updater artifact name for {target}"
        )));
    }
    Ok(name)
}

fn verify_release(
    config_path: &Path,
    manifest_path: &Path,
    assets_dir: &Path,
) -> Result<usize, VerificationError> {
    let config: TauriConfig = read_json(config_path, "Tauri release configuration")?;
    let decoded_public_key =
        decode_outer_base64(&config.plugins.updater.pubkey, "Tauri updater public key")?;
    let public_key = PublicKey::decode(&decoded_public_key)
        .map_err(|_| verification_error("Invalid Tauri updater public key"))?;
    let manifest: UpdateManifest = read_json(manifest_path, "Tauri update manifest")?;

    let actual_targets = manifest
        .platforms
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected_targets = UPDATE_TARGETS.into_iter().collect::<BTreeSet<_>>();
    if actual_targets != expected_targets {
        return Err(verification_error(
            "Update manifest must contain exactly the supported updater targets",
        ));
    }

    let mut artifact_names = BTreeSet::new();
    for target in UPDATE_TARGETS {
        let platform = manifest.platforms.get(target).ok_or_else(|| {
            verification_error(format!("Missing updater manifest entry for {target}"))
        })?;
        let name = artifact_name(&platform.url, target)?;
        if !artifact_names.insert(name.clone()) {
            return Err(verification_error(format!(
                "Duplicate updater artifact name for {target}"
            )));
        }

        let payload_path = assets_dir.join(&name);
        let payload = fs::read(&payload_path).map_err(|error| {
            verification_error(format!(
                "Could not read updater payload {}: {error}",
                payload_path.display()
            ))
        })?;
        let signature_path = assets_dir.join(format!("{name}.sig"));
        let adjacent_signature = fs::read_to_string(&signature_path)
            .map_err(|error| {
                verification_error(format!(
                    "Could not read updater signature {}: {error}",
                    signature_path.display()
                ))
            })?
            .trim()
            .to_owned();
        if adjacent_signature != platform.signature {
            return Err(verification_error(format!(
                "Manifest signature does not match the adjacent signature for {target}"
            )));
        }

        let decoded_signature = decode_outer_base64(
            &platform.signature,
            &format!("updater signature for {target}"),
        )?;
        let signature = Signature::decode(&decoded_signature)
            .map_err(|_| verification_error(format!("Invalid updater signature for {target}")))?;
        public_key.verify(&payload, &signature, true).map_err(|_| {
            verification_error(format!(
                "Updater signature verification failed for {target}"
            ))
        })?;
    }

    Ok(UPDATE_TARGETS.len())
}

fn run() -> Result<(), VerificationError> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let [config_path, manifest_path, assets_dir] = arguments.as_slice() else {
        return Err(verification_error(
            "Usage: coddy-studio-updater-verifier <tauri-release-config> <latest-json> <assets-dir>",
        ));
    };
    let verified = verify_release(
        Path::new(config_path),
        Path::new(manifest_path),
        Path::new(assets_dir),
    )?;
    println!("Verified {verified} updater payload signatures.");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Updater verification failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::{Value, json};
    use tempfile::TempDir;

    const TEST_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IGRldGVybWluaXN0aWMgdGVzdCBwdWJsaWMga2V5ClJXUUJBZ01FQlFZSENPcEtiR1BpbkZJS3Z2VlFleE11eGZtVlIzYXV2cjU3a2tJZTZta1VSdElzCg==";
    const TEST_SIGNATURE_A: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IGRldGVybWluaXN0aWMgdGVzdCBzaWduYXR1cmUKUlVRQkFnTUVCUVlIQ0h6NCtsNEhlOU0zb3VCRW82NWVZbmdTWTJvQ2hXRFZvQnZ0b0ZuQU9zRW5FeDU1Y1FIcEhuSGI5ZnAyTTBkbk0xbmcxYW1xcjFRSGZOV3ZVcWgvRndrPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDowXHRmaWxlOmEKQkRFTlA1NDVTc1oyMmM5ZjliTEZJR2dTNXliaFhJSi9rQUtGTkxRalRsVGQwOHFIbmhsVDkzUEM5OGY3cXBTaS9GbHM2SWZOVHNkdVdrM2RrN01zQnc9PQo=";
    const TEST_SIGNATURE_B: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IGRldGVybWluaXN0aWMgdGVzdCBzaWduYXR1cmUKUlVRQkFnTUVCUVlIQ0puVTArNDlzejJzdnhYVHNjUFhuME1ITzN1aTFibmxxNjRmbXBvQ1pjQzFvSjBoNnVQM3dTQWVnZGhLSzZPMkNORGlaemxuZDRZSUJJaGpBekhKU2dVPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDowXHRmaWxlOmIKOVNnWHBqRS9ZQU1ya0pKWWc3WWIxUmpLQjRKL0hXZ0FreTcrc093VEdHYU95dU1EWXFSQTl1ZDFkdTdZa2t1ek94Q2JuWE14NEcvQ2ppZjRtT1NOQWc9PQo=";

    struct Fixture {
        _temp: TempDir,
        assets_dir: std::path::PathBuf,
        config_path: std::path::PathBuf,
        manifest_path: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().expect("fixture directory");
        let assets_dir = temp.path().join("assets");
        std::fs::create_dir(&assets_dir).expect("assets directory");
        let config_path = temp.path().join("tauri.release.conf.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec(&json!({
                "plugins": {"updater": {"pubkey": TEST_PUBLIC_KEY}}
            }))
            .expect("configuration JSON"),
        )
        .expect("configuration fixture");

        let platforms = [
            ("darwin-aarch64", "payload-a-one", b"payload-a".as_slice(), TEST_SIGNATURE_A),
            ("darwin-x86_64", "payload-b-one", b"payload-b".as_slice(), TEST_SIGNATURE_B),
            ("linux-aarch64", "payload-a-three", b"payload-a".as_slice(), TEST_SIGNATURE_A),
            ("linux-x86_64", "payload-a-two", b"payload-a".as_slice(), TEST_SIGNATURE_A),
            ("windows-aarch64", "payload-b-three", b"payload-b".as_slice(), TEST_SIGNATURE_B),
            ("windows-x86_64", "payload-b-two", b"payload-b".as_slice(), TEST_SIGNATURE_B),
        ]
        .into_iter()
        .map(|(target, artifact, payload, signature)| {
            std::fs::write(assets_dir.join(artifact), payload).expect("payload fixture");
            std::fs::write(assets_dir.join(format!("{artifact}.sig")), signature)
                .expect("signature fixture");
            (
                target.to_owned(),
                json!({
                    "signature": signature,
                    "url": format!("https://github.com/mubeda/CoddyStudio/releases/download/v1.2.3/{artifact}")
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
        let manifest_path = assets_dir.join("latest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&json!({"platforms": platforms})).expect("manifest JSON"),
        )
        .expect("manifest fixture");

        Fixture {
            _temp: temp,
            assets_dir,
            config_path,
            manifest_path,
        }
    }

    fn rewrite_manifest(fixture: &Fixture, update: impl FnOnce(&mut Value)) {
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&fixture.manifest_path).expect("manifest"))
                .expect("manifest JSON");
        update(&mut manifest);
        std::fs::write(
            &fixture.manifest_path,
            serde_json::to_vec(&manifest).expect("manifest JSON"),
        )
        .expect("updated manifest");
    }

    #[test]
    fn verifies_every_manifest_payload_with_the_configured_public_key() {
        let fixture = fixture();

        assert_eq!(
            verify_release(
                Path::new(&fixture.config_path),
                Path::new(&fixture.manifest_path),
                Path::new(&fixture.assets_dir),
            )
            .expect("valid release signatures"),
            6
        );
    }

    #[test]
    fn rejects_a_wrong_public_key() {
        let fixture = fixture();
        let other_key = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        std::fs::write(
            &fixture.config_path,
            serde_json::to_vec(&json!({
                "plugins": {"updater": {"pubkey": STANDARD.encode(other_key)}}
            }))
            .expect("configuration JSON"),
        )
        .expect("wrong public-key fixture");

        assert!(
            verify_release(
                &fixture.config_path,
                &fixture.manifest_path,
                &fixture.assets_dir
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_a_modified_payload() {
        let fixture = fixture();
        std::fs::write(fixture.assets_dir.join("payload-a-one"), b"modified")
            .expect("modified payload fixture");

        assert!(
            verify_release(
                &fixture.config_path,
                &fixture.manifest_path,
                &fixture.assets_dir
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_swapped_otherwise_valid_signatures() {
        let fixture = fixture();
        rewrite_manifest(&fixture, |manifest| {
            manifest["platforms"]["darwin-aarch64"]["signature"] = json!(TEST_SIGNATURE_B);
            manifest["platforms"]["darwin-x86_64"]["signature"] = json!(TEST_SIGNATURE_A);
        });
        std::fs::write(
            fixture.assets_dir.join("payload-a-one.sig"),
            TEST_SIGNATURE_B,
        )
        .expect("swapped signature A");
        std::fs::write(
            fixture.assets_dir.join("payload-b-one.sig"),
            TEST_SIGNATURE_A,
        )
        .expect("swapped signature B");

        assert!(
            verify_release(
                &fixture.config_path,
                &fixture.manifest_path,
                &fixture.assets_dir
            )
            .is_err()
        );
    }
}
