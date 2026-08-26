use cubic_version::{
    CompatibilityProfileId, MinecraftVersionId, MinecraftVersionKind, ProtocolVersion,
    VersionCatalog, VersionData, VersionDataFormatVersion, VersionError, deserialize_catalog,
    deserialize_version_data, serialize_catalog, serialize_version_data,
};

fn data(id: &str, kind: MinecraftVersionKind, protocol: i32, profiles: &[&str]) -> VersionData {
    VersionData::new(
        MinecraftVersionId::new(id).unwrap(),
        kind,
        ProtocolVersion::new(protocol),
        profiles
            .iter()
            .map(|profile| CompatibilityProfileId::new(*profile).unwrap())
            .collect(),
    )
    .unwrap()
}

#[test]
fn current_format_is_accepted_and_exposed() {
    let parsed = deserialize_version_data(
        br#"{"format_version":1,"minecraft_version":"test","kind":"release","protocol":7}"#,
    )
    .unwrap();
    assert_eq!(parsed.format_version(), VersionDataFormatVersion::CURRENT);
    assert_eq!(parsed.kind(), MinecraftVersionKind::Release);
}

#[test]
fn unsupported_format_is_rejected_before_typed_payload_parsing() {
    let error = deserialize_version_data(br#"{"format_version":2}"#).unwrap_err();
    assert!(matches!(
        error,
        VersionError::UnsupportedFormatVersion {
            found: 2,
            supported: 1
        }
    ));
}

#[test]
fn malformed_missing_and_invalid_kind_json_are_structured_errors() {
    assert!(matches!(
        deserialize_version_data(b"{"),
        Err(VersionError::MalformedJson { .. })
    ));
    assert!(matches!(
        deserialize_version_data(br#"{"format_version":1,"kind":"release"}"#),
        Err(VersionError::InvalidField { .. })
    ));
    assert!(matches!(
        deserialize_version_data(
            br#"{"format_version":1,"minecraft_version":"test","kind":"previewish","protocol":7}"#
        ),
        Err(VersionError::InvalidField { .. })
    ));
}

#[test]
fn invalid_protocol_and_compatibility_metadata_are_rejected() {
    assert!(matches!(
        VersionData::new(
            MinecraftVersionId::new("test").unwrap(),
            MinecraftVersionKind::Release,
            ProtocolVersion::new(-1),
            Vec::new()
        ),
        Err(VersionError::InvalidProtocolVersion { value: -1 })
    ));
    assert!(deserialize_version_data(
        br#"{"format_version":1,"minecraft_version":"test","kind":"release","protocol":7,"compatibility_profiles":["BAD/profile"]}"#
    )
    .is_err());
}

#[test]
fn duplicate_profile_and_catalog_ids_are_rejected() {
    let profile = CompatibilityProfileId::new("same").unwrap();
    assert!(matches!(
        VersionData::new(
            MinecraftVersionId::new("test").unwrap(),
            MinecraftVersionKind::Release,
            ProtocolVersion::new(7),
            vec![profile.clone(), profile]
        ),
        Err(VersionError::DuplicateCompatibilityProfile { .. })
    ));
    assert!(matches!(
        deserialize_catalog(
            br#"{"format_version":1,"versions":[{"minecraft_version":"same","kind":"release","protocol":1},{"minecraft_version":"same","kind":"snapshot","protocol":2}]}"#
        ),
        Err(VersionError::DuplicateVersionId { .. })
    ));
}

#[test]
fn serialization_is_canonical_newline_terminated_and_byte_identical() {
    let first = data(
        "synthetic",
        MinecraftVersionKind::Snapshot,
        42,
        &["z-profile", "a-profile"],
    );
    let second = data(
        "synthetic",
        MinecraftVersionKind::Snapshot,
        42,
        &["a-profile", "z-profile"],
    );
    let first_bytes = serialize_version_data(&first).unwrap();
    assert_eq!(first_bytes, serialize_version_data(&second).unwrap());
    assert!(first_bytes.ends_with(b"\n"));
}

#[test]
fn catalog_order_and_protocol_matches_are_deterministic() {
    let z = data("z-release", MinecraftVersionKind::Release, 7, &[]);
    let a = data("a-snapshot", MinecraftVersionKind::Snapshot, 7, &[]);
    let catalog = VersionCatalog::from_versions([&z, &a]).unwrap();
    let ids: Vec<_> = catalog
        .entries()
        .iter()
        .map(|entry| entry.minecraft_version().as_str())
        .collect();
    assert_eq!(ids, ["a-snapshot", "z-release"]);
    assert_eq!(catalog.find_by_protocol(ProtocolVersion::new(7)).count(), 2);
    assert_eq!(
        serialize_catalog(&catalog).unwrap(),
        serialize_catalog(&catalog).unwrap()
    );
}
