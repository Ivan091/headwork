# Vendored files

`headwork dash` builds these into the binary (`crates/headwork/src/ops/dash.rs`, `ASSETS`), so the page makes no
request outside the server and works offline. Each file is the published package file, unchanged, downloaded from
`https://cdn.jsdelivr.net/npm/<package>@<version>/<path>` on 2026-11-23. All four are MIT licensed; each file carries
its licence header or names it in its package.

| File | Package | Version | SHA-256 |
|---|---|---|---|
| `vendor/cytoscape.min.js` | `cytoscape` (`dist/cytoscape.min.js`) | 3.30.2 | `83e8c54a6bec655bfd81df07df605649c268af69aeca67a5ea2da54ea42dac81` |
| `vendor/layout-base.js` | `layout-base` | 2.0.1 | `ec15ab5df9af3f20708f4faab994accf91cda71848cd5bb10a23432cc50b6745` |
| `vendor/cose-base.js` | `cose-base` | 2.2.0 | `7cae9509bd36235a63a85e71c8d9fa2cd0bc1d0c1ecc5b5a737976f39d040ddf` |
| `vendor/cytoscape-fcose.js` | `cytoscape-fcose` | 2.2.0 | `4b1cab218d74996aa59cd8473f9239cc6398b8c1774d84d7e59ad9a68959cb57` |

To update one: download the new version to the same path, put its version and `sha256sum` here, and check the page.
A test (`vendored_files_match_their_checksums` in `ops/dash.rs`) compares the built-in bytes with this table.
