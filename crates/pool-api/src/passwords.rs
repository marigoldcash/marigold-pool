//! Miners' passwords: choosing one, hashing it, checking it. Only the hash is
//! ever stored, and a password never appears in a log or a reply.

use argon2::password_hash::{SaltString, rand_core::OsRng};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_PASSWORD_CHARS: usize = 128;

/// A password a miner may choose, or a sentence saying why not.
pub fn check_new_password(password: &str) -> Result<(), String> {
    let length = password.chars().count();
    if length < MIN_PASSWORD_CHARS {
        return Err(format!("Choose a password of at least {MIN_PASSWORD_CHARS} characters."));
    }
    if length > MAX_PASSWORD_CHARS {
        return Err(format!("That password is too long. Use at most {MAX_PASSWORD_CHARS} characters."));
    }
    Ok(())
}

/// Hash a password with a fresh random salt. Slow on purpose: run it off the
/// async threads.
pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(password.as_bytes(), &salt).map(|hash| hash.to_string()).map_err(|e| e.to_string())
}

/// Whether `password` is the one that made `stored`. Anything unreadable counts as wrong.
pub fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored).ok().is_some_and(|hash| Argon2::default().verify_password(password.as_bytes(), &hash).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_must_be_a_sensible_length() {
        assert!(check_new_password("1234567").unwrap_err().contains("at least 8"));
        assert!(check_new_password("12345678").is_ok());
        assert!(check_new_password(&"a".repeat(128)).is_ok());
        assert!(check_new_password(&"a".repeat(129)).unwrap_err().contains("at most 128"));
        assert!(check_new_password("éééééééé").is_ok(), "length is counted in characters, not bytes");
    }

    #[test]
    fn a_hash_checks_the_right_password_and_nothing_else() {
        let hash = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("correct horsf", &hash));
        assert!(!verify_password("", &hash));
        assert!(!hash.contains("correct horse"), "the password is not in what is stored");
    }

    #[test]
    fn the_same_password_hashes_differently_each_time() {
        assert_ne!(hash_password("same password").unwrap(), hash_password("same password").unwrap(), "each has its own salt");
    }

    #[test]
    fn something_that_is_not_a_hash_never_matches() {
        assert!(!verify_password("anything", "not a hash"));
        assert!(!verify_password("anything", ""));
    }
}
