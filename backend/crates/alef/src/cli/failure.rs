// SPDX-License-Identifier: MIT OR Apache-2.0
//! The failure categories of the binary: usage (2), declined (3), runtime (1).
use alef_core::error::AlefError;
use alef_launch::application::{pipeline::SettleFailure, source::LoadFailure, trust::TrustFailure};
use alef_launch::runtimes::StoreError;

/// Exit codes: 2 — the command line or the manifest is unusable, 3 — the user has not given the
/// rights the application asks for, 1 — the runtime failed.
pub(crate) enum Failure {
    Usage(String),
    Declined(String),
    Runtime(String),
}

impl Failure {
    /// The category of a source that cannot be loaded, at its own exit code.
    pub(crate) fn load(failure: LoadFailure) -> Self {
        match failure.exit {
            2 => Self::Usage(failure.to_string()),
            3 => Self::Declined(failure.to_string()),
            _ => Self::Runtime(failure.to_string()),
        }
    }

    /// Runtime store errors retain their category and operational/usage exit code.
    pub(crate) fn runtime_store(error: StoreError) -> Self {
        if error.exit() == 2 {
            Self::Usage(error.to_string())
        } else {
            Self::Runtime(error.to_string())
        }
    }

    /// Trust that was not established: unusable input, refusal, or a store failure.
    pub(crate) fn trust(failure: TrustFailure) -> Self {
        match failure {
            TrustFailure::BadTrustKey(message) => {
                Self::Usage(format!("INVALID_ARGUMENT: {message}"))
            }
            TrustFailure::Refused(message) => Self::Declined(message),
            TrustFailure::Store(error) => Self::Runtime(error.to_string()),
        }
    }
}

impl From<SettleFailure> for Failure {
    /// The shared pipeline: documents, trust, consent, enforcement.
    fn from(failure: SettleFailure) -> Self {
        match failure {
            SettleFailure::Documents(failure) => Self::load(failure),
            SettleFailure::Trust(failure) => Self::trust(failure),
            SettleFailure::Unanswered(unanswered) => Self::Declined(unanswered.to_string()),
            SettleFailure::Store(error) => Self::Runtime(error.to_string()),
            SettleFailure::Enforcement(message) => Self::Runtime(message),
        }
    }
}

impl From<AlefError> for Failure {
    fn from(error: AlefError) -> Self {
        Self::Usage(format!("{}: {}", error.code.as_str(), error.message))
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Runtime(error.to_string())
    }
}

pub(crate) fn install_failure(error: alef_launch::install::InstallError) -> Failure {
    match error.exit() {
        2 => Failure::Usage(error.to_string()),
        3 => Failure::Declined(error.to_string()),
        _ => Failure::Runtime(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_launch::consent::{Unanswered, Why};

    #[test]
    fn a_load_failure_maps_to_its_own_exit_code_and_prints_its_category() {
        for (category, exit) in [
            (Some("NOT_A_PACKAGE"), 2),
            (Some("PACKAGE_FORMAT"), 2),
            (Some("PACKAGE_INTEGRITY"), 2),
            (Some("PACKAGE_SIGNATURE"), 2),
            (Some("PACKAGE_MANIFEST"), 2),
            (Some("MANIFEST_INVALID"), 2),
            (Some("RUNTIME_INCOMPATIBLE"), 2),
            (Some("PACKAGE_RUN_LIMIT"), 2),
            (Some("SIGNATURE_REQUIRED"), 3),
            (None, 1),
        ] {
            let failure = Failure::load(LoadFailure {
                category,
                detail: "detail".to_owned(),
                exit,
                runtime_requirement: None,
            });
            match &failure {
                Failure::Usage(message) if exit == 2 => {
                    if let Some(category) = category {
                        assert!(message.contains(category), "{message}");
                    }
                }
                Failure::Declined(message) if exit == 3 => {
                    assert!(message.contains("SIGNATURE_REQUIRED"), "{message}");
                    assert!(message.contains("detail"), "{message}");
                }
                Failure::Runtime(message) if exit == 1 => {
                    assert!(category.is_none());
                    assert!(message.contains("detail"), "{message}");
                }
                _ => panic!("exit {exit}: wrong variant"),
            }
        }
    }

    #[test]
    fn runtime_store_failures_preserve_categories_and_usage_or_operational_exit_codes() {
        for (error, category, exit) in [
            (
                StoreError::InvalidArgument("invalid".to_owned()),
                "INVALID_ARGUMENT",
                2,
            ),
            (
                StoreError::AlreadyInstalled("present".to_owned()),
                "ALREADY_INSTALLED",
                2,
            ),
            (
                StoreError::Store(std::io::Error::other("unreadable")),
                "RUNTIME_STORE",
                1,
            ),
        ] {
            assert_eq!(error.exit(), exit);
            match Failure::runtime_store(error) {
                Failure::Usage(message) if exit == 2 => {
                    assert!(message.starts_with(&format!("{category}: ")))
                }
                Failure::Runtime(message) if exit == 1 => {
                    assert_eq!(message, "RUNTIME_STORE: unreadable")
                }
                _ => panic!("wrong failure variant for exit {exit}"),
            }
        }
    }

    #[test]
    fn a_failed_relaunch_start_is_an_operational_runtime_relaunch_failed_failure() {
        let Failure::Runtime(message) =
            Failure::Runtime("RUNTIME_RELAUNCH_FAILED: denied".to_owned())
        else {
            panic!("a start failure is operational")
        };
        assert!(
            message.starts_with("RUNTIME_RELAUNCH_FAILED: "),
            "{message}"
        );
    }

    #[test]
    fn a_trust_failure_maps_to_unusable_refused_or_operational() {
        let Failure::Usage(message) = Failure::trust(TrustFailure::BadTrustKey("x".to_owned()))
        else {
            panic!("a bad trust key is unusable input");
        };
        assert!(message.contains("INVALID_ARGUMENT: x"), "{message}");
        let Failure::Declined(message) =
            Failure::trust(TrustFailure::Refused("SIGNER_CHANGED: r".to_owned()))
        else {
            panic!("a refusal is declined");
        };
        assert!(message.contains("SIGNER_CHANGED"), "{message}");
        let Failure::Runtime(message) = Failure::trust(TrustFailure::Store(std::io::Error::new(
            std::io::ErrorKind::Other,
            "io",
        ))) else {
            panic!("a store failure is operational");
        };
        assert!(message.contains("io"), "{message}");
    }

    #[test]
    fn a_settlement_failure_maps_to_its_own_exit_code_family() {
        let Failure::Usage(message) = Failure::from(SettleFailure::Documents(LoadFailure {
            category: Some("MANIFEST_INVALID"),
            detail: "assets: /index.html: missing".to_owned(),
            exit: 2,
            runtime_requirement: None,
        })) else {
            panic!("a document failure is unusable input");
        };
        assert!(message.contains("MANIFEST_INVALID"), "{message}");
        let Failure::Usage(_) = Failure::from(SettleFailure::Trust(TrustFailure::BadTrustKey(
            "k".to_owned(),
        ))) else {
            panic!("a bad trust key is unusable input");
        };
        let Failure::Declined(_) = Failure::from(SettleFailure::Unanswered(Unanswered {
            rights: Vec::new(),
            why: Why::Nobody,
        })) else {
            panic!("unanswered rights are declined");
        };
        let Failure::Runtime(_) = Failure::from(SettleFailure::Store(std::io::Error::new(
            std::io::ErrorKind::Other,
            "io",
        ))) else {
            panic!("a store failure is operational");
        };
        let Failure::Runtime(_) = Failure::from(SettleFailure::Enforcement("e".to_owned())) else {
            panic!("an enforcement failure is operational");
        };
    }
}
