pub(crate) fn coddy_studio_env_var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name)
}

pub(crate) fn coddy_studio_env_string(name: &str) -> Option<String> {
    coddy_studio_env_var(name).and_then(|value| value.into_string().ok())
}
