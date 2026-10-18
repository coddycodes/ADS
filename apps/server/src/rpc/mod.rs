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
    E2EE_HANDSHAKE_TIMEOUT, E2eeChannel, E2eePreauthAdmission, E2eePreauthLease, E2eeSessionError,
    MAX_E2EE_CIPHERTEXT_BYTES, accept_noise_nk, effective_preauth_peer, run_e2ee_session,
};
// Only the unix media-socket tests read these.
#[cfg(all(test, unix))]
pub(crate) use e2ee::{
    E2EE_HOST_IDENTITY_CLOSE_CODE, E2EE_RECORD_FLAG_CONTINUATION, E2EE_RECORD_FLAG_FINAL,
    MAX_E2EE_CHUNK_BYTES,
};
pub(crate) use methods::{MethodMutability, method_mutability};
#[cfg(test)]
pub(crate) use session::PairingConfirmationLatch;
pub(crate) use session::{
    PreparedRpcResponse, RpcResponseEnqueueGuard, RpcResponseEnqueuePermit, RpcSessionContext,
    RpcUnaryResult, encoded_server_message_len, response_too_large_failure, run_session,
};
pub(crate) use transport::{
    CHUNKED_RPC_SUBPROTOCOL, ConnectionLiveness, HeartbeatTiming, MAX_RECORDED_MESSAGE_BYTES,
    watch_heartbeat,
};
