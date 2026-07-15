use russh::keys::{HashAlg, PublicKey};

const TEST_AUTHORIZED_KEYS: &[&str] = &[
    "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC7Pa0R64MB2Qk0VRqFqT4c4iJVHWPrj4K6TLQ7O8LbKGNNyqPdQu1MbJZVYcyvvqqh/BS4VwUt2/Cv/V7eff9VcwU0TFX3dhfuezjXH9WIRaev4tJgn88FgpxGQI5cVzQyWErzMteOL3OPAiJe8C+xYSrowlTZcfE6jkyIu37RM5/oWwg/Gkf07l4kaiz8YOoM00Dvuvn/FVC5Rk2aqn4fZLyF7KFSCkTCCY+2tWJZDko3EXHy+AxBBS4Tq5r7+h6cAEBOVjgnFh3fUYpGiSJuwNWsUOanGKmMprX9ZcgUNpJZcpGDBGlWYDLEWm2ThAZT5CBSL3a+2SkMZVVSi1kn dcagent_key",
];

/// Check if a public key is permitted to log in.
pub fn is_authorized_public_key(public_key: &PublicKey) -> bool {
    let fp = public_key.fingerprint(HashAlg::Sha256);
    TEST_AUTHORIZED_KEYS
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
