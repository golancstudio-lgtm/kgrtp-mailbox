mod authentication;
mod client;
mod player_auth;

pub use client::{map_transport, AuthKind, CallMode, UgsClient, UgsError};
pub use player_auth::{encode_path, PlayerAuth, PlayerRecord};
