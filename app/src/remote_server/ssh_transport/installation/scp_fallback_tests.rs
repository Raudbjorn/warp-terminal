use remote_server::setup::{RemoteArch, RemoteOs};

use super::*;

#[test]
fn tarball_cache_separates_download_sources_and_channels() {
    let platform = RemotePlatform {
        os: RemoteOs::Linux,
        arch: RemoteArch::X86_64,
    };
    let stable =
        InstallScriptOptions::new("https://downloads.example.test/a".into(), "stable".into());
    let other_source =
        InstallScriptOptions::new("https://downloads.example.test/b".into(), "stable".into());
    let preview = InstallScriptOptions::new(stable.download_base_url.clone(), "preview".into());
    let stable_path = remote_server_tarball_cache_path(&platform, &stable);

    // Reusing a version from another source or channel bypasses the configured download policy.
    assert_ne!(
        stable_path,
        remote_server_tarball_cache_path(&platform, &other_source)
    );
    assert_ne!(
        stable_path,
        remote_server_tarball_cache_path(&platform, &preview)
    );
    assert_eq!(
        stable_path,
        remote_server_tarball_cache_path(&platform, &stable)
    );
}
