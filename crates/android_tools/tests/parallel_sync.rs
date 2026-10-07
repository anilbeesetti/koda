/*
 * Copyright (C) 2024 The Android Open Source Project
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

use android_tools::{
    generated_artifacts::{ArtifactModelVersions, CapturedField, GetterUnavailable, ModelVersion},
    parallel_sync::supports_parallel_sync,
    project_tree_facts::FactsUnavailableReason,
};

fn versions(
    major: i32,
    minor: i32,
    description: &str,
    agp: &str,
) -> CapturedField<ArtifactModelVersions> {
    CapturedField::Available(ArtifactModelVersions {
        agp: agp.into(),
        producer: ModelVersion {
            major,
            minor,
            description: Some(description.into()),
        },
        // The original helper passes minimumModelConsumer=null; this feature ignores it.
        minimum_consumer: CapturedField::Unavailable(GetterUnavailable {
            capability: "minimumModelConsumer".into(),
            detail: "No value supplied by the original reference helper".into(),
        }),
        models: Vec::new(),
    })
}

fn original_supports(agp: &str) -> bool {
    supports_parallel_sync(&versions(i32::MIN, i32::MIN, "", agp))
        .expect("Original helper supplies a valid AGP version")
}

// Complete ModelVersionsTest.checkSupportsParallelSync; see unchanged attributed fixture.
#[test]
fn check_supports_parallel_sync() {
    let assert_supports = |agp: &str| {
        assert!(original_supports(agp), "AGP {agp} supports parallel sync");
    };
    let assert_does_not_support = |agp: &str| {
        assert!(!original_supports(agp), "AGP {agp} supports parallel sync");
    };
    assert_does_not_support("7.2.0-rc01");
    assert_supports("7.2.0");
    assert_supports("7.2.1");
    assert_supports("7.2.2");
    assert_does_not_support("7.3.0-alpha01");
    assert_does_not_support("7.3.0-alpha02");
    assert_does_not_support("7.3.0-alpha03");
    assert_supports("7.3.0-alpha04");
    assert_supports("7.3.0");
}

#[test]
fn exact_agp_ranges_preserve_preview_and_micro_boundaries() {
    for (agp, expected) in [
        ("7.1.9", false),
        ("7.2.0-alpha99", false),
        ("7.2.0-beta01", false),
        ("7.2.0-rc99", false),
        ("7.2.0-dev", false),
        ("7.2.1-alpha01", true),
        ("7.2.99-rc01", true),
        ("7.3.0-alpha00", true),
        ("7.3.0-alpha99", true),
        ("7.3.0-beta00", true),
        ("7.3.0-rc01", true),
        ("7.3.0-dev", true),
        ("7.3.1-alpha01", true),
        ("8.0.0-dev", true),
    ] {
        assert_eq!(original_supports(agp), expected, "AGP {agp}");
    }
}

#[test]
fn every_valid_73_alpha_observes_the_exact_excluded_interval() {
    for preview in 0..=99 {
        let agp = format!("7.3.0-alpha{preview:02}");
        assert_eq!(
            original_supports(&agp),
            !(1..4).contains(&preview),
            "AGP {agp}"
        );
    }
}

#[test]
fn explicit_model_override_preserves_signed_order_and_descriptions() {
    for (major, minor, expected) in [
        (i32::MIN, i32::MIN, false),
        (7, i32::MAX, false),
        (8, -1, false),
        (8, 0, true),
        (9, i32::MIN, true),
        (i32::MAX, i32::MAX, true),
    ] {
        for description in ["", "z", "Android Gradle Plugin producer"] {
            let input = versions(major, minor, description, "7.3.0-alpha03");
            assert_eq!(supports_parallel_sync(&input).unwrap(), expected);
            assert_eq!(
                input.available().unwrap().producer.description.as_deref(),
                Some(description)
            );
        }
    }
}

#[test]
fn missing_and_not_applicable_versions_remain_typed_errors() {
    let missing = CapturedField::Unavailable(GetterUnavailable {
        capability: "agpV2Versions".into(),
        detail: "Official SDK version getter is absent".into(),
    });
    let error = supports_parallel_sync(&missing).unwrap_err();
    assert_eq!(error.reason, FactsUnavailableReason::Capability);
    assert!(error.detail.contains("agpV2Versions"));
    assert!(
        error
            .detail
            .contains("Official SDK version getter is absent")
    );
    let absent = CapturedField::NotApplicable("No Android tooling version capability".into());
    let error = supports_parallel_sync(&absent).unwrap_err();
    assert_eq!(error.reason, FactsUnavailableReason::UnsupportedShape);
    assert_eq!(error.detail, "No Android tooling version capability");
}

#[test]
fn malformed_agp_is_not_masked_by_new_model_versions() {
    for agp in [
        "",
        "7.2",
        "7.2.0.1",
        "07.2.0",
        "7.02.0",
        "7.2.00",
        "7.2.0-alpha1",
        "7.2.0-alpha007",
        "7.2.0-rc",
        "7.2.0-alpha01-extra",
        "7.2.0+metadata",
        " 7.2.0",
        "7.2.0\n",
        "7.2.0-alpha０１",
        "2147483648.0.0",
        "7.2147483648.0",
        "7.2.2147483648",
    ] {
        for producer in [i32::MIN, i32::MAX] {
            let input = versions(producer, 0, "", agp);
            assert_eq!(
                supports_parallel_sync(&input).unwrap_err().reason,
                FactsUnavailableReason::Malformed,
                "AGP {agp:?}, producer {producer}"
            );
        }
    }
}

#[test]
fn unknown_agp_qualifiers_remain_unsupported_with_new_model_versions() {
    for agp in ["7.2.0-snapshot", "7.2.0-canary01", "7.2.0-DEV"] {
        for producer in [i32::MIN, i32::MAX] {
            assert_eq!(
                supports_parallel_sync(&versions(producer, 0, "", agp))
                    .unwrap_err()
                    .reason,
                FactsUnavailableReason::UnsupportedShape
            );
        }
    }
}

#[test]
fn historical_preview_spelling_matches_the_pinned_parser() {
    for agp in [
        "3.0.0-alpha1",
        "3.1.0-beta1",
        "3.1.0-alpha01",
        "3.1.0-rc01",
        "3.1.1-alpha1",
        "3.2.0-beta01",
    ] {
        assert!(!original_supports(agp), "Valid historical AGP {agp}");
    }
    for agp in [
        "3.0.0-alpha01",
        "3.1.0-beta01",
        "3.1.0-alpha1",
        "3.1.0-rc1",
        "3.1.1-alpha01",
        "3.2.0-beta1",
    ] {
        assert_eq!(
            supports_parallel_sync(&versions(i32::MIN, 0, "", agp))
                .unwrap_err()
                .reason,
            FactsUnavailableReason::Malformed,
            "Invalid historical AGP {agp}"
        );
    }
}

#[test]
fn signed_int_agp_extremes_are_valid_without_overflow() {
    assert!(!original_supports("0.0.0"));
    assert!(original_supports("2147483647.2147483647.2147483647"));
}

#[test]
fn long_invalid_input_has_bounded_work_and_error_output() {
    let input = "9".repeat(100_000);
    let error = supports_parallel_sync(&versions(i32::MAX, i32::MAX, "", &input)).unwrap_err();
    assert_eq!(error.reason, FactsUnavailableReason::Malformed);
    assert!(error.detail.len() < 200);
    assert!(!error.detail.contains(&input));
}
