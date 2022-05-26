use anyhow::Result;
use serde::{Deserialize, Serialize};

use std::fmt::{self, Display};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

pub type Str = smallstr::SmallString<[u8; 32]>;

impl Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "session({})", self.0)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    NewSession,

    AttachSession(SessionId),
    AttachSessionByAlias(Str),

    KillSession { sid: SessionId, force: bool },
    KillSessionByAlias { alias: Str, force: bool },

    AliasSession { sid: SessionId, alias: Str },

    ListSessions,
    StopServer { force: bool },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    NewSession(SessionId),
    ListSessions(Vec<(SessionId, std::time::SystemTime, Str)>),
    Ok,
    Err,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SessionRequest {
    Terminate,
    Detach,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SessionResponse {
    Terminated(bool),
    Err,
    Ok,
}

pub fn addr() -> std::path::PathBuf {
    let tmpdir = std::env::temp_dir();
    tmpdir.join(format!("hxd-{}.sock", env!("CARGO_PKG_VERSION")))
}
