#[test]
fn app_can_use_core_startup_message() {
    assert_eq!(
        cubic_core::startup_message(),
        "Cubic starting...\nPhase 1 repository scaffold initialized."
    );
}
