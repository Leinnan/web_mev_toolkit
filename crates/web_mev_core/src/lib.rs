//! Framework-free helpers shared by web apps.
//!
//! Nothing here knows about axum, sqlx or Tera. Password hashing is behind the
//! `password` feature, because it needs tokio for its worker pool.

pub mod clock;
pub mod env;
pub mod ip;
#[cfg(feature = "password")]
pub mod password;
pub mod token;
pub mod validate;

pub use clock::{Clock, FixedClock, SystemClock, unix_now};
pub use env::{Env, EnvError};
pub use ip::client_ip;
#[cfg(feature = "password")]
pub use password::{PasswordError, PasswordHasher};
pub use token::{bearer, random_token, secure_eq, sha256_hex, token_hash};
pub use validate::{ValidationError, required_text, slugify, validate_slug};
