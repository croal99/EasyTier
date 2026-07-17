use russh::keys::{HashAlg, PublicKey};

/// Check if a public key is permitted to log in.
/// Keys are provided by the caller (loaded from `SshConfig`).
pub fn is_authorized_public_key(
    public_key: &PublicKey,
    config_keys: &[String],
) -> bool {
    let fp = public_key.fingerprint(HashAlg::Sha256);
    config_keys
        .iter()
        .filter_map(|line| parse_authorized_key(line))
        .any(|k| k.fingerprint(HashAlg::Sha256) == fp)
}

/// Parse a single OpenSSH authorized_keys line to a PublicKey.
fn parse_authorized_key(line: &str) -> Option<PublicKey> {
    let mut it = line.split_whitespace();
    let first = it.next()?;
    let token = it.next().unwrap_or(first);
    russh::keys::parse_public_key_base64(token).ok()
}
