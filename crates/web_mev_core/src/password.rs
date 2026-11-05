//! Argon2id password hashing that can't stall the async runtime.
//!
//! A hash takes tens of milliseconds of CPU and 19 MiB of memory, so it runs on
//! the blocking pool behind a semaphore. Logins for unknown accounts should
//! call [`PasswordHasher::verify_or_dummy`], so they take as long as real ones
//! and don't reveal which accounts exist.

use std::{ops::RangeInclusive, sync::Arc};

use argon2::{Argon2, PasswordHash, PasswordHasher as _, PasswordVerifier as _};
use tokio::sync::{OnceCell, Semaphore};

/// Why a password couldn't be hashed or checked.
#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    /// The password's length is outside the limits. The message is safe to
    /// display.
    #[error("password must contain between {min} and {max} characters")]
    Length {
        /// Fewest characters allowed.
        min: usize,
        /// Most characters allowed.
        max: usize,
    },
    /// Hashing failed, or its worker died. Not safe to display.
    #[error("password hashing failed: {0}")]
    Internal(String),
}

/// Bounded Argon2id hashing. Clones share one worker limit.
#[derive(Debug, Clone)]
pub struct PasswordHasher {
    workers: Arc<Semaphore>,
    dummy: Arc<OnceCell<String>>,
    length: RangeInclusive<usize>,
}

impl Default for PasswordHasher {
    /// At most four hashes at once, and passwords of 12 to 1024 characters.
    fn default() -> Self {
        Self::new(4, 12..=1024)
    }
}

impl PasswordHasher {
    /// A hasher running at most `workers` hashes at once and accepting new
    /// passwords whose character count is in `length`. The upper bound stops a
    /// long password being used to waste CPU.
    #[must_use]
    pub fn new(workers: usize, length: RangeInclusive<usize>) -> Self {
        Self {
            workers: Arc::new(Semaphore::new(workers.max(1))),
            dummy: Arc::new(OnceCell::new()),
            length,
        }
    }

    /// Checks a new password's length without hashing it.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError::Length`] when it is outside the limits.
    pub fn check_length(&self, password: &str) -> Result<(), PasswordError> {
        if self.length.contains(&password.chars().count()) {
            Ok(())
        } else {
            Err(PasswordError::Length {
                min: *self.length.start(),
                max: *self.length.end(),
            })
        }
    }

    /// Hashes a new password, enforcing the length limits.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError::Length`] when it is outside the limits, and
    /// [`PasswordError::Internal`] when hashing fails.
    pub async fn hash(&self, password: String) -> Result<String, PasswordError> {
        self.check_length(&password)?;
        self.hash_unchecked(password).await
    }

    async fn hash_unchecked(&self, password: String) -> Result<String, PasswordError> {
        self.blocking(move || {
            Argon2::default()
                .hash_password(password.as_bytes())
                .map(|hash| hash.to_string())
                .map_err(|error| PasswordError::Internal(error.to_string()))
        })
        .await?
    }

    /// Checks a password against a stored hash. A malformed hash verifies as
    /// `false`, and length limits don't apply, so old passwords keep working
    /// after a limit changes.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError::Internal`] when the worker dies.
    pub async fn verify(&self, password: String, hash: String) -> Result<bool, PasswordError> {
        self.blocking(move || {
            PasswordHash::new(&hash).is_ok_and(|hash| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok()
            })
        })
        .await
    }

    /// Checks a password against the account's hash, or against a throwaway
    /// hash when there is no account, and returns `false` then. Both paths do
    /// the same work.
    ///
    /// # Errors
    ///
    /// Returns [`PasswordError::Internal`] when hashing fails.
    pub async fn verify_or_dummy(
        &self,
        password: String,
        hash: Option<String>,
    ) -> Result<bool, PasswordError> {
        if let Some(hash) = hash {
            return self.verify(password, hash).await;
        }
        let dummy = self
            .dummy
            .get_or_try_init(|| self.hash_unchecked("unknown identity dummy".into()))
            .await?
            .clone();
        self.verify(password, dummy).await?;
        Ok(false)
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, PasswordError> {
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| PasswordError::Internal(error.to_string()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| PasswordError::Internal(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::{PasswordError, PasswordHasher};

    #[tokio::test]
    async fn hash_and_verify_round_trip() {
        let hasher = PasswordHasher::default();
        let hash = hasher.hash("a long enough secret".into()).await.unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(
            hasher
                .verify("a long enough secret".into(), hash.clone())
                .await
                .unwrap()
        );
        assert!(
            !hasher
                .verify("the wrong secret here".into(), hash)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn malformed_hash_verifies_false() {
        let hasher = PasswordHasher::default();
        assert!(
            !hasher
                .verify("anything".into(), "not a hash".into())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn length_limits_count_characters() {
        let hasher = PasswordHasher::new(1, 4..=5);
        assert!(matches!(
            hasher.hash("abc".into()).await,
            Err(PasswordError::Length { min: 4, max: 5 })
        ));
        assert!(hasher.hash("abcdef".into()).await.is_err());
        assert!(hasher.hash("żółty".into()).await.is_ok());
    }

    #[tokio::test]
    async fn short_old_passwords_still_verify() {
        let hash = PasswordHasher::new(1, 1..=99)
            .hash("short".into())
            .await
            .unwrap();
        assert!(
            PasswordHasher::default()
                .verify("short".into(), hash)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn unknown_account_does_the_work_and_fails() {
        let hasher = PasswordHasher::default();
        assert!(
            !hasher
                .verify_or_dummy("whatever at all".into(), None)
                .await
                .unwrap()
        );
        assert!(hasher.dummy.get().is_some());
        let hash = hasher.hash("a long enough secret".into()).await.unwrap();
        assert!(
            hasher
                .verify_or_dummy("a long enough secret".into(), Some(hash))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn worker_limit_holds() {
        let hasher = PasswordHasher::new(2, 1..=99);
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let hasher = hasher.clone();
            let running = running.clone();
            let peak = peak.clone();
            tasks.push(tokio::spawn(async move {
                hasher
                    .blocking(move || {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(20));
                        running.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await
                    .unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }
}
