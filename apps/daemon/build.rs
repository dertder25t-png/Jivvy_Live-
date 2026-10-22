//! Release builds must carry the production license public key (see src/license.rs).
//!
//! `JIVVY_LICENSE_PUBLIC_KEY`: the raw 32-byte Ed25519 public key, base64url (43 characters).
//! A release build without it, or with the published test key, fails here, unless the
//! `unlicensed-release` feature marks it as a CI test build (which then takes no keys at all).

/// The published test key (packages/protocol/fixtures/license.json); never in a release.
const TEST_PUBLIC_KEY: &str = "wByFzMtLdt_jsvriieoeMsBjJzQfr6GZUo-NXPwDhyI";

fn well_formed(key: &str) -> bool {
    let b64 = key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    // 32 bytes leave the last character's two low bits unused, so it is one of these.
    key.len() == 43 && b64 && key.ends_with(|c| "AEIMQUYcgkosw048".contains(c))
}

fn main() {
    println!("cargo:rerun-if-env-changed=JIVVY_LICENSE_PUBLIC_KEY");
    println!("cargo:rerun-if-changed=build.rs");
    let key = std::env::var("JIVVY_LICENSE_PUBLIC_KEY").unwrap_or_default();
    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let opted_out = std::env::var_os("CARGO_FEATURE_UNLICENSED_RELEASE").is_some();
    if !key.is_empty() && !well_formed(&key) {
        panic!("JIVVY_LICENSE_PUBLIC_KEY must be a raw Ed25519 public key, base64url without padding (43 characters)");
    }
    if release && key == TEST_PUBLIC_KEY {
        panic!("JIVVY_LICENSE_PUBLIC_KEY is the published test key; release builds need the production key");
    }
    if release && key.is_empty() && !opted_out {
        panic!(
            "a release build needs JIVVY_LICENSE_PUBLIC_KEY (the production license public key, apps/daemon/README.md); \
             CI test builds pass --features unlicensed-release instead"
        );
    }
}
