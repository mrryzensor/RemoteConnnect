pub mod crypto;
pub mod messages;

pub use crypto::{E2eSession, EphemeralKeyPair, hash_pin, verify_pin};
pub use messages::*;

/// Generate a human-readable 9-digit device ID (e.g., "482910374").
pub fn generate_device_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let num: u32 = rng.gen_range(100_000_000..=999_999_999);
    num.to_string()
}

/// Format a 9-digit device ID as "123 456 789".
pub fn format_device_id(id: &str) -> String {
    let digits: String = id.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 9 {
        format!("{} {} {}", &digits[0..3], &digits[3..6], &digits[6..9])
    } else {
        id.to_string()
    }
}

/// Normalize a device ID by stripping whitespace and hyphens.
pub fn normalize_device_id(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// Generate a random 6-digit PIN for quick access.
pub fn generate_pin() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let num: u32 = rng.gen_range(100_000..=999_999);
    num.to_string()
}
