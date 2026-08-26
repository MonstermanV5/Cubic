use cubic_version::{CompatibilityProfileId, MinecraftVersionId, VersionError};

#[test]
fn release_and_snapshot_style_ids_are_opaque_and_valid() {
    assert_eq!(
        MinecraftVersionId::new("1.21.4").unwrap().as_str(),
        "1.21.4"
    );
    assert_eq!(
        MinecraftVersionId::new("26.1.2").unwrap().as_str(),
        "26.1.2"
    );
    assert_eq!(
        MinecraftVersionId::new("26w34a").unwrap().as_str(),
        "26w34a"
    );
}

#[test]
fn empty_dot_and_traversal_ids_are_rejected() {
    for value in ["", ".", "..", "../release", "release/../other"] {
        assert!(matches!(
            MinecraftVersionId::new(value),
            Err(VersionError::InvalidVersionId { .. })
        ));
    }
}

#[test]
fn separators_controls_and_overlong_ids_are_rejected() {
    for value in [
        "release/one",
        "release\\one",
        "release\0one",
        "release\none",
    ] {
        assert!(MinecraftVersionId::new(value).is_err());
    }
    assert!(MinecraftVersionId::new("x".repeat(129)).is_err());
}

#[test]
fn cross_platform_reserved_path_components_are_rejected() {
    for value in ["CON", "nul.json", "release:one", "release?", "trailing."] {
        assert!(MinecraftVersionId::new(value).is_err());
    }
}

#[test]
fn compatibility_profile_ids_use_a_stable_restricted_namespace() {
    assert_eq!(
        CompatibilityProfileId::new("synthetic-layout_v1")
            .unwrap()
            .as_str(),
        "synthetic-layout_v1"
    );
    for value in ["", "Uppercase", "path/profile", "has space", "🔥"] {
        assert!(matches!(
            CompatibilityProfileId::new(value),
            Err(VersionError::InvalidCompatibilityProfileId { .. })
        ));
    }
}
