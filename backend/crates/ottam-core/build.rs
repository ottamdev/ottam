fn main() {
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=src/storage/payload/migrations");

    // `test_seams`: the environment overrides tests and the UI harness drive
    // (test agents, download redirects, an in-memory secret store). Every
    // debug build has them; an optimized build has them only with the
    // `test-seams` feature (`build-app.sh --profile`), so a release core
    // cannot be told by its environment to run code it did not ship.
    println!("cargo:rustc-check-cfg=cfg(test_seams)");
    if std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some()
        || std::env::var_os("CARGO_FEATURE_TEST_SEAMS").is_some()
    {
        println!("cargo:rustc-cfg=test_seams");
    }
}
