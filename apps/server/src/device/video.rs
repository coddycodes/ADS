//! H.264 units every video backend produces and the media hub fans out.

#[derive(Clone, Debug, PartialEq)]
pub enum UnitKind {
    Config,
    Key,
    Delta,
}

#[derive(Clone, Debug)]
pub struct VideoUnit {
    pub kind: UnitKind,
    pub pts_us: u64,
    pub data: bytes::Bytes,
}
