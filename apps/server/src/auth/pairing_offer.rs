//! Pairing-offer input rules shared by `POST /api/auth/pairing-offer` and the
//! `coddy-studio pairing offer` CLI, so the two paths cannot drift.

use thiserror::Error;

use super::{
    HOST_IDENTITY_SECRET_NAME, HostIdentity, HostIdentityError, SecretStore, SecretStoreError,
    issue_offline_share_pairing,
    pairing_code::{
        PairingCodeError, REMOTE_PAIRING_CODE_VERSION, RemotePairingCodePayload,
        RemotePairingReach, encode_pairing_code, pairing_deep_link,
    },
    service::{AuthError, PAIRING_REACH_VALUES, is_loopback_host, is_unspecified_host},
};
use crate::persistence::{Repositories, StorageInstanceId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedPairingOfferInput {
    pub(crate) name: String,
    pub(crate) endpoint: String,
    pub(crate) reach: String,
    pub(crate) off_host: bool,
    pub(crate) label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub(crate) enum PairingOfferInputError {
    #[error("endpoint must be an http(s) URL")]
    Endpoint,
    #[error("endpoint must be a connectable address (no wildcard host, no port 0)")]
    Unconnectable,
    #[error("reach does not match the offered endpoint")]
    Reach,
}

pub(crate) fn validate_pairing_offer_input(
    name: &str,
    endpoint: &str,
    reach: &str,
    label: Option<&str>,
) -> Result<ValidatedPairingOfferInput, PairingOfferInputError> {
    let endpoint_raw = endpoint.trim();
    let parsed = match url::Url::parse(endpoint_raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        _ => return Err(PairingOfferInputError::Endpoint),
    };
    let host = parsed.host_str().unwrap_or_default();
    if host.is_empty() || is_unspecified_host(host) || parsed.port() == Some(0) {
        return Err(PairingOfferInputError::Unconnectable);
    }
    let endpoint_is_loopback = is_loopback_host(host);
    if !PAIRING_REACH_VALUES.contains(&reach) {
        return Err(PairingOfferInputError::Reach);
    }
    let reach_ok = match reach {
        "this-computer" => endpoint_is_loopback,
        "another-device" => !endpoint_is_loopback,
        _ => true,
    };
    let name = name.trim();
    if !reach_ok || name.is_empty() {
        return Err(PairingOfferInputError::Reach);
    }
    let off_host = match reach {
        "another-device" => true,
        "this-computer" => false,
        _ => !endpoint_is_loopback,
    };
    let label = label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or(name)
        .to_owned();
    Ok(ValidatedPairingOfferInput {
        name: name.to_owned(),
        endpoint: endpoint_raw.trim_end_matches('/').to_owned(),
        reach: reach.to_owned(),
        off_host,
        label,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OfflinePairingOffer {
    pub(crate) id: String,
    pub(crate) code: String,
    pub(crate) link: String,
    pub(crate) reach: String,
    pub(crate) endpoint: String,
    pub(crate) name: String,
    pub(crate) expires_at: String,
}

#[derive(Debug, Error)]
pub(crate) enum OfflinePairingOfferError {
    #[error("no persisted host identity; start the server on this data root first")]
    MissingHostIdentity,
    #[error(transparent)]
    SecretStore(#[from] SecretStoreError),
    #[error(transparent)]
    HostIdentity(#[from] HostIdentityError),
    #[error("could not issue the pairing grant: {0:?}")]
    Auth(AuthError),
    #[error(transparent)]
    Encode(#[from] PairingCodeError),
}

/// Mints a share offer beside (or without) a running server. Reads the host
/// identity instead of generating it so the CLI can never create a key a live
/// server has not loaded.
pub(crate) async fn mint_offline_pairing_offer(
    repositories: &Repositories,
    secret_store: &SecretStore,
    storage_instance_id: StorageInstanceId,
    input: ValidatedPairingOfferInput,
) -> Result<OfflinePairingOffer, OfflinePairingOfferError> {
    let record = secret_store
        .get(HOST_IDENTITY_SECRET_NAME)
        .await?
        .ok_or(OfflinePairingOfferError::MissingHostIdentity)?;
    let host_identity = HostIdentity::from_record(&record)?;
    let issued = issue_offline_share_pairing(
        repositories,
        Some(input.label.clone()),
        input.reach.clone(),
        input.off_host,
    )
    .await
    .map_err(OfflinePairingOfferError::Auth)?;
    let payload = RemotePairingCodePayload {
        v: REMOTE_PAIRING_CODE_VERSION,
        endpoint: input.endpoint.clone(),
        name: input.name.clone(),
        token: issued.credential,
        host_key: host_identity.public_key_base64url(),
        reach: match input.reach.as_str() {
            "this-computer" => RemotePairingReach::ThisComputer,
            "another-device" => RemotePairingReach::AnotherDevice,
            _ => RemotePairingReach::Custom,
        },
        storage_instance_id: storage_instance_id.to_string(),
    };
    let code = encode_pairing_code(&payload)?;
    Ok(OfflinePairingOffer {
        id: issued.id,
        link: pairing_deep_link(&code),
        code,
        reach: input.reach,
        endpoint: input.endpoint,
        name: input.name,
        expires_at: issued.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_off_host_endpoint_for_another_device() {
        let input = validate_pairing_offer_input(
            " ai-server ",
            "http://100.105.196.60:3773/",
            "another-device",
            None,
        )
        .expect("valid input");
        assert_eq!(input.name, "ai-server");
        assert_eq!(input.endpoint, "http://100.105.196.60:3773");
        assert_eq!(input.reach, "another-device");
        assert!(input.off_host);
        assert_eq!(input.label, "ai-server");
    }

    #[test]
    fn uses_a_trimmed_label_when_present() {
        let input = validate_pairing_offer_input(
            "ai-server",
            "http://100.105.196.60:3773",
            "another-device",
            Some("  laptop  "),
        )
        .expect("valid input");
        assert_eq!(input.label, "laptop");
    }

    #[test]
    fn rejects_loopback_for_another_device_and_off_host_for_this_computer() {
        let error =
            validate_pairing_offer_input("x", "http://127.0.0.1:3773", "another-device", None)
                .expect_err("loopback is not another device");
        assert_eq!(
            error.to_string(),
            "reach does not match the offered endpoint"
        );
        let error =
            validate_pairing_offer_input("x", "http://10.0.0.5:3773", "this-computer", None)
                .expect_err("off-host is not this computer");
        assert_eq!(
            error.to_string(),
            "reach does not match the offered endpoint"
        );
        let input =
            validate_pairing_offer_input("x", "http://127.0.0.1:3773", "this-computer", None)
                .expect("loopback this-computer is valid");
        assert!(!input.off_host);
        let input = validate_pairing_offer_input("x", "https://proxy.example.com", "custom", None)
            .expect("custom accepts any host");
        assert!(input.off_host);
    }

    #[test]
    fn rejects_bad_endpoints_and_names() {
        assert_eq!(
            validate_pairing_offer_input("x", "ftp://host:1", "custom", None)
                .expect_err("scheme")
                .to_string(),
            "endpoint must be an http(s) URL"
        );
        assert_eq!(
            validate_pairing_offer_input("x", "http://0.0.0.0:3773", "custom", None)
                .expect_err("wildcard")
                .to_string(),
            "endpoint must be a connectable address (no wildcard host, no port 0)"
        );
        assert_eq!(
            validate_pairing_offer_input("x", "http://10.0.0.5:0", "custom", None)
                .expect_err("port zero")
                .to_string(),
            "endpoint must be a connectable address (no wildcard host, no port 0)"
        );
        assert_eq!(
            validate_pairing_offer_input("   ", "http://10.0.0.5:3773", "custom", None)
                .expect_err("blank name")
                .to_string(),
            "reach does not match the offered endpoint"
        );
        assert_eq!(
            validate_pairing_offer_input("x", "http://10.0.0.5:3773", "everywhere", None)
                .expect_err("unknown reach")
                .to_string(),
            "reach does not match the offered endpoint"
        );
    }

    #[tokio::test]
    async fn mints_a_share_shaped_grant_against_a_prepared_root() {
        let temp = tempfile::tempdir().expect("temporary base directory");
        let mut config = crate::config::ServerConfig::new(temp.path());
        let resolved = crate::resolve_data_root(config.data_root_request.clone())
            .expect("resolve test data root");
        config.base_dir = resolved.effective.clone();
        config.resolved_data_root = Some(resolved);
        crate::persistence::StatePaths::from_config(&config)
            .ensure_directories_without_database_side_effects()
            .await
            .expect("state directories");
        let prepared = crate::persistence::prepare_store(&config)
            .await
            .expect("prepare a fresh store");
        let repositories = crate::persistence::Repositories::new(prepared.database.clone());
        let secret_store = crate::auth::SecretStore::new(&prepared.paths.secrets_dir)
            .await
            .expect("secret store");
        let missing = mint_offline_pairing_offer(
            &repositories,
            &secret_store,
            prepared.storage_instance_id,
            validate_pairing_offer_input(
                "ai-server",
                "http://10.0.0.5:3773",
                "another-device",
                None,
            )
            .expect("valid input"),
        )
        .await
        .expect_err("no host identity yet");
        assert!(matches!(
            missing,
            OfflinePairingOfferError::MissingHostIdentity
        ));

        let identity = crate::auth::HostIdentity::load_or_generate(&secret_store)
            .await
            .expect("generate a host identity like the server does");
        let offer = mint_offline_pairing_offer(
            &repositories,
            &secret_store,
            prepared.storage_instance_id,
            validate_pairing_offer_input(
                "ai-server",
                "http://10.0.0.5:3773",
                "another-device",
                Some("laptop"),
            )
            .expect("valid input"),
        )
        .await
        .expect("mint offer");
        assert!(offer.link.starts_with("coddy-studio://pair?code="));
        let payload = crate::auth::pairing_code::decode_pairing_code(&offer.code).expect("decodes");
        assert_eq!(payload.endpoint, "http://10.0.0.5:3773");
        assert_eq!(payload.name, "ai-server");
        assert_eq!(payload.host_key, identity.public_key_base64url());
        assert_eq!(
            payload.storage_instance_id,
            prepared.storage_instance_id.to_string()
        );
        assert_eq!(
            payload.reach,
            crate::auth::pairing_code::RemotePairingReach::AnotherDevice
        );

        let active = repositories
            .list_active_auth_pairing_links(crate::auth::service::format_iso(
                crate::auth::service::now_ms(),
            ))
            .await
            .expect("active links");
        let link = active
            .iter()
            .find(|link| link.id == offer.id)
            .expect("persisted grant");
        assert_eq!(link.subject, "one-time-token");
        assert_eq!(link.reach.as_deref(), Some("another-device"));
        assert_eq!(link.off_host, Some(true));
        assert_eq!(link.label.as_deref(), Some("laptop"));
        assert!(
            link.scopes
                .as_array()
                .expect("scope array")
                .iter()
                .any(|scope| scope == "orchestration:read")
        );
        assert!(
            !link
                .scopes
                .as_array()
                .expect("scope array")
                .iter()
                .any(|scope| scope == "access:write")
        );
        prepared.database.close().await;
    }
}
