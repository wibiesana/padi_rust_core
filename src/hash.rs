//! Password hashing helpers (PHP password_hash / password_verify equivalent, bcrypt).
pub fn make(password: &str) -> String {
    bcrypt::hash(password, bcrypt::DEFAULT_COST).unwrap_or_default()
}

pub fn verify(password: &str, hash: &str) -> bool {
    bcrypt::verify(password, hash).unwrap_or(false)
}
