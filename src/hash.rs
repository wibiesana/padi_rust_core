//! Password hashing and verification utilities using bcrypt.

/// Hash a password using bcrypt with the default cost.
pub fn make(password: &str) -> String {
    bcrypt::hash(password, bcrypt::DEFAULT_COST).unwrap_or_default()
}

/// Idiomatic alias for `make`.
pub fn hash(password: &str) -> String {
    make(password)
}

/// Verify a password against a bcrypt hash.
pub fn verify(password: &str, hash: &str) -> bool {
    bcrypt::verify(password, hash).unwrap_or(false)
}
