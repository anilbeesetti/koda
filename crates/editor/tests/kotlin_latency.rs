use gpui::TestAppContext;
use settings::SettingsStore;

// Keep the editor dependency free of cfg(test) wrapping invariants during release measurements.
#[gpui::test]
#[ignore = "Requires ZED_KOTLIN_CLIENT_PROBE_CONFIG and a real Kotlin LSP installation"]
async fn real_kotlin_latency(cx: &mut TestAppContext) {
    cx.update(|cx| {
        assets::Assets.load_test_fonts(cx);
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        release_channel::init(semver::Version::new(0, 0, 0), cx);
        editor::init(cx);
    });
    zlog::init_test();
    editor::test::run_real_kotlin_editor_probe(cx).await;
}
