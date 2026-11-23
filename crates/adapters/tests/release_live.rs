//! Asks GitHub for headwork's newest release through the real `curl`, with the token `gh` holds: run with
//! `cargo test -p headwork-adapters --test release_live -- --ignored`.

#![expect(clippy::unwrap_used, reason = "a live test fails loudly")]

use headwork_adapters::release::{Curl, Fetched, ReleaseSource};

#[test]
#[ignore = "reaches GitHub"]
fn the_newest_release_comes_back_with_the_discovered_token() {
    let curl = Curl::discovered();
    let fetched = curl.latest("Ivan091/headwork", None).unwrap();
    let Fetched::Fresh { release, etag } = fetched else {
        unreachable!("no ETag was sent")
    };
    assert!(release.tag.starts_with("v0.1."), "{}", release.tag);
    assert_eq!(
        curl.latest("Ivan091/headwork", etag.as_deref()).unwrap(),
        Fetched::NotModified,
        "the ETag still matches"
    );
}
