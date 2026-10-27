use astrolabe::store::ObjectKey;

#[test]
fn object_keys_render_as_namespace_slash_name() {
    assert_eq!(
        ObjectKey::new(Some("team-a".to_string()), "nightly").to_string(),
        "team-a/nightly"
    );
    assert_eq!(ObjectKey::cluster_scoped("default").to_string(), "default");
}
