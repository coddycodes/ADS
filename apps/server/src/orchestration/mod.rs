pub mod delivery;
pub mod engine;

pub use delivery::{
    AttachmentReference, CommandAdmission, NewProviderTurnDelivery, ProviderTurnDelivery,
    TurnDeliveryFailureReason, TurnDeliveryMode, TurnDeliveryState, TurnDeliveryTransition,
    canonical_command_digest,
};
pub use engine::{
    EngineOptions, OrchestrationCommand, OrchestrationEngine, OrchestrationError, Snapshot,
    load_shell_snapshot, load_snapshot, load_thread_snapshot,
};

#[cfg(test)]
mod delivery_tests {
    use serde_json::json;

    use super::delivery::canonical_command_digest;

    #[test]
    fn canonical_digest_sorts_keys_and_binds_attachment_content() {
        let left = json!({"b":2,"a":{"y":2,"x":1}});
        let right = json!({"a":{"x":1,"y":2},"b":2});
        assert_eq!(
            canonical_command_digest(&left).unwrap(),
            canonical_command_digest(&right).unwrap()
        );
        assert_ne!(
            canonical_command_digest(&json!({"dataUrl":"data:text/plain;base64,YQ=="})).unwrap(),
            canonical_command_digest(&json!({"dataUrl":"data:text/plain;base64,Yg=="})).unwrap(),
        );
    }
}
