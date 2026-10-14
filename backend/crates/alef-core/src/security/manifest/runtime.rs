// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runtime compatibility and package-only version validation.

use semver::{Version, VersionReq};

pub(super) fn deserialize_runtime<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    struct Runtime;
    impl<'de> serde::de::Visitor<'de> for Runtime {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a nonempty semver VersionReq String")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            // semver 1.0.28 VersionReq::parse is the grammar oracle, not npm ranges.
            VersionReq::parse(value).map_err(E::custom)?;
            Ok(value.to_owned())
        }
    }
    // deserialize_any rejects null and Ktav's numeric-to-String coercion.
    d.deserialize_any(Runtime)
        .map(Some)
        .map_err(|e| serde::de::Error::custom(format!("runtime: {e}")))
}

/// Compare strict SemVer strings by precedence. Build metadata does not affect ordering.
pub fn compare_versions(left: &str, right: &str) -> Result<std::cmp::Ordering, semver::Error> {
    Ok(Version::parse(left)?.cmp_precedence(&Version::parse(right)?))
}

/// Check a runtime requirement against a strict SemVer runtime version.
///
/// Uses exactly `semver::VersionReq` grammar and matching semantics: comma-separated
/// intersections, caret (including bare versions), tilde, comparators and wildcards.
/// Disjunctions (`||`), npm hyphen ranges and space-only intersections are invalid.
/// Prereleases require an explicit comparator for that major/minor/patch; build
/// metadata does not affect matching. Returns false if either input is invalid.
/// This pure helper neither selects nor discovers an installed runtime.
pub fn runtime_requirement_met(requirement: &str, runtime_version: &str) -> bool {
    match (
        VersionReq::parse(requirement),
        Version::parse(runtime_version),
    ) {
        (Ok(requirement), Ok(version)) => requirement.matches(&version),
        _ => false,
    }
}

/// Validate a package's version with strict `semver::Version::parse`.
///
/// Requires all three numeric components and accepts SemVer prerelease/build
/// identifiers. No trimming, coercion or prefix removal is performed. Directory
/// manifest version validation remains nonempty-only; callers opt into this
/// package-only rule explicitly. Returns the parsed version or the parser error.
pub fn validate_package_version(version: &str) -> Result<Version, semver::Error> {
    Version::parse(version)
}

#[cfg(test)]
mod tests;
