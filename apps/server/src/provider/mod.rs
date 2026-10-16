pub(crate) mod acp_mode;
pub(crate) mod attachments;
pub mod claude;
pub mod codex;
pub mod cursor;
pub(crate) mod environment;
pub mod grok;
pub mod opencode;

use std::fmt;

/// Driver-owned capability used by both inventory and durable steer admission.
pub(crate) fn supports_turn_steer(provider: &str) -> bool {
    matches!(provider, "codex" | "claudeAgent")
}

/// A provider slash invocation, `/name arguments`, as `(name, arguments)`. A name is ASCII
/// letters, digits, `-`, `_`, `:` and `.`, so text such as a leading file path is not one.
pub(crate) fn parse_provider_command(text: &str) -> Option<(&str, &str)> {
    let command = text.strip_prefix('/')?;
    let split = command.find(char::is_whitespace).unwrap_or(command.len());
    let name = &command[..split];
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        return None;
    }
    Some((name, command[split..].trim()))
}

/// Driver-owned capability used by both inventory and checkpoint revert: the driver's
/// `rollback` drops the newest turns from its conversation. A revert on any other provider
/// restores only the files.
pub(crate) fn supports_conversation_rewind(provider: &str) -> bool {
    matches!(provider, "codex" | "opencode")
}

// The one refusal policy. Every driver builds the sentence an undelivered turn shows from these
// helpers. `option` is the option's label where the driver has one, otherwise its id.

/// The selected model refuses `option`.
pub(crate) fn unsupported_option_refusal(option: &str) -> String {
    format!("{option} is not supported by the selected model.")
}

/// A turn option carries no id.
pub(crate) fn option_without_id_refusal() -> String {
    "The turn has an option without an id.".to_owned()
}

/// A select or text option carries no usable value.
pub(crate) fn option_needs_value_refusal(option: &str) -> String {
    format!("{option} needs a value.")
}

/// An on/off option carries something other than on or off.
pub(crate) fn option_on_or_off_refusal(option: &str) -> String {
    format!("{option} must be on or off.")
}

/// Two options exclude each other.
pub(crate) fn options_conflict_refusal(first: &str, second: &str) -> String {
    format!("{first} and {second} can't be used together.")
}

/// A turn carries options but no model to apply them to.
pub(crate) fn options_need_model_refusal() -> String {
    "Options need a selected model.".to_owned()
}

/// The provider does not offer the turn's model. `provider_label` is the instance's label.
pub(crate) fn model_unavailable_refusal(model: &str, provider_label: &str) -> String {
    format!("{model} is not available in {provider_label}.")
}

/// The session's own option configuration is one BiBCode can't work with, so no option can be
/// applied to it. The configuration is protocol detail the user can't act on, so the sentence
/// stays generic and names what the user can do instead.
pub(crate) fn unusable_options_refusal() -> String {
    "BiBCode can't apply these options to the selected model. Choose another model, or turn these \
     options off."
        .to_owned()
}

/// A live session can't switch to its provider's default model, because the session advertises
/// no default it can return to.
pub(crate) fn default_model_unavailable_refusal() -> String {
    "This session can't switch to the default model. Choose another model.".to_owned()
}

/// A deterministic refusal of a turn's options: the same request is refused again on every
/// retry, so a durable delivery that meets one fails once instead of retrying. `detail` is the
/// technical text that logs and RPC errors show (the Display); `refusal` is the plain sentence an
/// undelivered turn shows, built with the helpers above.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefusedOption {
    pub(crate) detail: String,
    pub(crate) refusal: String,
}

impl RefusedOption {
    pub(crate) fn new(detail: impl Into<String>, refusal: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            refusal: refusal.into(),
        }
    }

    /// A refusal caused by a session configuration BiBCode can't work with rather than by the
    /// request, shown as [`unusable_options_refusal`].
    pub(crate) fn unusable(detail: impl Into<String>) -> Self {
        Self::new(detail, unusable_options_refusal())
    }
}

impl fmt::Display for RefusedOption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl OptionRefusal for RefusedOption {
    fn refused_option(&self) -> Option<&RefusedOption> {
        Some(self)
    }
}

/// Implemented by each driver runtime's error type, so one mapping turns a refusal into an
/// option refusal of the provider runtime and leaves every other error retryable.
pub trait OptionRefusal {
    /// The refusal behind this error; `None` for every other error, such as a failed request,
    /// which a retry may overcome.
    fn refused_option(&self) -> Option<&RefusedOption>;

    /// The plain sentence an undelivered turn shows for a refusal.
    fn option_refusal(&self) -> Option<&str> {
        self.refused_option()
            .map(|refused| refused.refusal.as_str())
    }
}
