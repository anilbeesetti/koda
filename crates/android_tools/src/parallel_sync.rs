/*
 * Copyright (C) 2022 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use crate::{
    generated_artifacts::{ArtifactModelVersions, CapturedField},
    project_tree_facts::{FactsUnavailable, FactsUnavailableReason},
};

// Every spelling accepted by the pinned signed-Int grammar fits within 40 bytes.
const MAX_AGP_VERSION_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PreviewKind {
    Alpha,
    Beta,
    Rc,
    Dev,
    Stable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AgpVersion {
    numbers: (i32, i32, i32),
    kind: PreviewKind,
    preview: u8,
}

/// Evaluate the pinned SUPPORTS_PARALLEL_SYNC feature; no Gradle requests are scheduled.
pub fn supports_parallel_sync(
    versions: &CapturedField<ArtifactModelVersions>,
) -> Result<bool, FactsUnavailable> {
    let versions = versions.available()?;
    // Invalid or unavailable explicit AGP facts must not be masked by a newer producer.
    let agp = parse_agp(&versions.agp)?;
    let stable_72 = AgpVersion {
        numbers: (7, 2, 0),
        kind: PreviewKind::Stable,
        preview: 0,
    };
    let alpha_73_first = AgpVersion {
        numbers: (7, 3, 0),
        kind: PreviewKind::Alpha,
        preview: 1,
    };
    let alpha_73_fourth = AgpVersion {
        preview: 4,
        ..alpha_73_first
    };
    Ok((versions.producer.major, versions.producer.minor) >= (8, 0)
        || (agp >= stable_72 && agp < alpha_73_first)
        || agp >= alpha_73_fourth)
}

fn unavailable(reason: FactsUnavailableReason, detail: impl Into<String>) -> FactsUnavailable {
    FactsUnavailable {
        reason,
        detail: detail.into(),
        path: None,
    }
}

fn parse_agp(version: &str) -> Result<AgpVersion, FactsUnavailable> {
    if version.len() > MAX_AGP_VERSION_BYTES {
        return Err(unavailable(
            FactsUnavailableReason::Malformed,
            "AGP version exceeds the bounded version-input size",
        ));
    }
    let malformed = || {
        unavailable(
            FactsUnavailableReason::Malformed,
            format!("Malformed AGP version {version:?}"),
        )
    };
    let mut parts = version.split('-');
    let numeric = parts.next().ok_or_else(malformed)?;
    let qualifier = parts.next();
    if parts.next().is_some() {
        return Err(malformed());
    }
    let parse_number = |value: &str| {
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(malformed());
        }
        value.parse::<i32>().map_err(|_| malformed())
    };
    let mut numbers = numeric.split('.');
    let major = parse_number(numbers.next().ok_or_else(malformed)?)?;
    let minor = parse_number(numbers.next().ok_or_else(malformed)?)?;
    let micro = parse_number(numbers.next().ok_or_else(malformed)?)?;
    if numbers.next().is_some() {
        return Err(malformed());
    }
    let (kind, preview) = match qualifier {
        None => (PreviewKind::Stable, 0),
        Some("dev") => (PreviewKind::Dev, 0),
        Some(value) => {
            let (kind, suffix) = if let Some(suffix) = value.strip_prefix("alpha") {
                (PreviewKind::Alpha, suffix)
            } else if let Some(suffix) = value.strip_prefix("beta") {
                (PreviewKind::Beta, suffix)
            } else if let Some(suffix) = value.strip_prefix("rc") {
                (PreviewKind::Rc, suffix)
            } else {
                return Err(unavailable(
                    FactsUnavailableReason::UnsupportedShape,
                    format!("Unsupported AGP version qualifier in {version:?}"),
                ));
            };
            if suffix.is_empty()
                || suffix.len() > 2
                || !suffix.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(malformed());
            }
            // Historical 3.0 and 3.1 preview spellings follow the exact pinned rule.
            let two_digits = major > 3
                || (major == 3 && minor > 1)
                || (major == 3 && minor == 1 && micro == 0 && kind != PreviewKind::Beta);
            if (two_digits && suffix.len() != 2) || (!two_digits && suffix.starts_with('0')) {
                return Err(malformed());
            }
            (kind, suffix.parse::<u8>().map_err(|_| malformed())?)
        }
    };
    Ok(AgpVersion {
        numbers: (major, minor, micro),
        kind,
        preview,
    })
}
