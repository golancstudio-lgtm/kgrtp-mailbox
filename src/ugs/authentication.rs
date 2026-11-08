use base64::Engine;

pub fn basic_authorization_header(key_id: &str, secret: &str) -> String {
    let raw = format!("{key_id}:{secret}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
    format!("Basic {encoded}")
}
