//! The property the packaging acceptance names: from a staged tree the
//! lookup resolves the bundle, its files match the source byte for byte,
//! and with no override a release run fails instead of resolving the
//! source checkout. The debug build's repo fallback is a different
//! property, so the case is release-only.

use std::path::Path;

use zeron_engine::{PLUGIN_DIR_ENV, plugin_dir};

/// The files a packaged dsh-captains bundle ships.
const SHIPPED_FILES: [&str; 3] = ["package.json", "cordis.patch.yml", "index.js"];

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "release builds only: the debug fallback is a different property"
)]
fn release_plugin_dir_resolves_the_packaged_bundle_byte_for_byte() {
    // This crate is apps/zeron, so the checkout's bundle is two directories up.
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/dsh-captains");
    for name in SHIPPED_FILES {
        assert!(
            source.join(name).is_file(),
            "the source bundle should ship {name}"
        );
    }

    // Stage the layout a package ships: dsh-plugins/dsh-captains.
    let temp = tempfile::tempdir().expect("a temp dir should stage the package");
    let staged = temp.path().join("dsh-plugins/dsh-captains");
    std::fs::create_dir_all(&staged).expect("the staged bundle directory should exist");
    for name in SHIPPED_FILES {
        std::fs::copy(source.join(name), staged.join(name))
            .expect("each shipped file should copy into the staged bundle");
    }

    // The test process owns its environment (nextest runs one test per process).
    unsafe { std::env::set_var(PLUGIN_DIR_ENV, &staged) };
    let resolved = plugin_dir().expect("plugin_dir should resolve the staged bundle");
    assert_eq!(
        std::fs::canonicalize(&resolved).expect("the resolved dir should canonicalize"),
        std::fs::canonicalize(&staged).expect("the staged dir should canonicalize"),
        "the lookup should return the staged bundle itself"
    );

    for name in SHIPPED_FILES {
        assert_eq!(
            std::fs::read(resolved.join(name)).expect("the resolved file should read"),
            std::fs::read(source.join(name)).expect("the source file should read"),
            "{name} differs between the resolved bundle and the source"
        );
    }

    // The test process owns its environment (nextest runs one test per process).
    unsafe { std::env::remove_var(PLUGIN_DIR_ENV) };
    assert!(
        source.join("package.json").is_file(),
        "the source checkout still exists, so a fallback would resolve it"
    );
    assert!(
        plugin_dir().is_none(),
        "a release run must fail rather than fall back to the source checkout"
    );
}
