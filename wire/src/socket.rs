//! The broker's socket (ADR-040): one JSON line asked (`Request`), JSON lines back (`Frame`), the
//! broker's exit code last. Who is asking is never in the request: the broker takes it from the kernel.

use serde::{Deserialize, Serialize};

/// How the broker says no approval could be asked (the desktop's agent is gone): a caller may repair it and ask again.
pub const NO_APPROVAL_AGENT: &str = "no authentication agent is running";

/// Where the socket is (modules/broker.nix sets the unit to it).
pub const SOCKET: &str = "/run/torq/broker.sock";

// How the broker ended: the last frame of every reply, and its exit code when run directly.
/// Done.
pub const OK: i32 = 0;
/// Refused, or failed: why is in its output.
pub const REFUSED: i32 = 1;
/// It did not understand the request (its arguments): to a newer caller, an older broker.
pub const USAGE: i32 = 2;
/// Another operation is running: try again later (EX_TEMPFAIL).
pub const BUSY: i32 = 75;
/// This needs the caller's password: none was sent, or it was wrong (EX_NOPERM).
pub const NEED_PASSWORD: i32 = 77;

/// What a caller sends: the broker's arguments, without who it is or any file. A diff or a password
/// hash comes in the request itself, never as a path for root to open (N16). An unknown field is
/// refused, not ignored: a caller that means something the broker does not know is told so.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// A backup's key (`backup-key`): its passphrase, kept root's alone, never in the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// What comes back, one JSON line at a time: the broker's standard output and error as they come,
/// and how it ended, last.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Frame {
    Out(String),
    Err(String),
    Exit(i32),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes on the socket, pinned: both ends of a running machine may be different builds.
    #[test]
    fn the_lines_on_the_socket() {
        let r = Request { args: vec!["log".into()], password: Some("pw".into()), ..Default::default() };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"args":["log"],"password":"pw"}"#);
        assert_eq!(serde_json::to_string(&Frame::Out("x\n".into())).unwrap(), r#"{"out":"x\n"}"#);
        assert_eq!(serde_json::to_string(&Frame::Err("e".into())).unwrap(), r#"{"err":"e"}"#);
        assert_eq!(serde_json::to_string(&Frame::Exit(NEED_PASSWORD)).unwrap(), r#"{"exit":77}"#);
        let back: Request = serde_json::from_str(r#"{"args":["apply","3"],"diff":"d"}"#).unwrap();
        assert_eq!((back.args, back.diff), (vec!["apply".to_string(), "3".to_string()], Some("d".to_string())));
    }

    #[test]
    fn an_unknown_field_is_refused() {
        assert!(serde_json::from_str::<Request>(r#"{"args":[],"actor":"user"}"#).is_err());
    }
}
