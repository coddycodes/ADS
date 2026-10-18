mod byte_budget;
mod e2ee;
mod message;
mod methods;
mod session;
mod transport;

pub use message::{
    CauseItem, ClientMessage, InvalidRequestId, RequestId, RpcExit, RpcRequest, ServerMessage,
    WireMessage,
};
pub use methods::{ACTIVE_RPC_METHODS, MethodMode, RpcMethodSpec};
pub use session::{RpcRegistry, RpcResult, RpcStreamChunk};

pub(crate) use e2ee::{
    E2eePreauthAdmission, MAX_E2EE_CIPHERTEXT_BYTES, effective_preauth_peer,
    remote_compression_enabled, run_e2ee_session,
};
pub(crate) use methods::{MethodMutability, method_mutability};
#[cfg(test)]
pub(crate) use session::PairingConfirmationLatch;
pub(crate) use session::{
    PreparedRpcResponse, RpcResponseEnqueueGuard, RpcResponseEnqueuePermit, RpcSessionContext,
    RpcUnaryResult, encoded_server_message_len, response_too_large_failure, run_session,
};
pub(crate) use transport::{MAX_RECORDED_MESSAGE_BYTES, plain_subprotocols};
