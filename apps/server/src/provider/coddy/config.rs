use serde_json::Value;

/// Driver-specific configuration for one Coddy provider instance, decoded from
/// the opaque `providerInstances.<id>.config` envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CoddyInstanceConfig {
    pub(crate) binary_path: String,
    pub(crate) server_url: String,
    pub(crate) data_dir: String,
}

impl CoddyInstanceConfig {
    pub(crate) fn from_value(value: Option<&Value>, fallback_binary: &str) -> Self {
        let string = |key: &str| {
            value
                .and_then(|value| value.get(key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };

        Self {
            binary_path: string("binaryPath").unwrap_or_else(|| fallback_binary.to_owned()),
            server_url: string("serverUrl").unwrap_or_default(),
            data_dir: string("dataDir").unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn partial_config_falls_back_to_the_driver_binary() {
        let config = CoddyInstanceConfig::from_value(
            Some(&json!({ "binaryPath": " /opt/coddy ", "serverUrl": "" })),
            "coddy",
        );

        assert_eq!(config.binary_path, "/opt/coddy");
        assert_eq!(config.server_url, "");
        assert_eq!(config.data_dir, "");
    }

    #[test]
    fn absent_config_uses_defaults() {
        let config = CoddyInstanceConfig::from_value(None, "coddy");

        assert_eq!(config.binary_path, "coddy");
        assert_eq!(config.server_url, "");
        assert_eq!(config.data_dir, "");
    }
}
