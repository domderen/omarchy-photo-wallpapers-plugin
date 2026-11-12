# Protocol and code attribution

`crates/wallpapers/src/icloud/srp.rs` adapts the Apple SRP arithmetic from
[kei](https://github.com/rhoopr/kei), revision
`3b8443b493845d408e36ac267af8360f2665d5e0`, Copyright 2026 Rob Hooper,
under the MIT license in `kei-MIT.txt`. This application does not invoke kei.
The frozen SRP fixture was independently calculated with Python's standard
library using the server-side SRP expression, synthetic credentials and fixed
private numbers.

Apple's consumer iCloud Photos web endpoints are undocumented, version-sensitive
interfaces, rather than a supported public Photos API. Authentication and Photos
request shapes were inspected in kei. Trusted-device PCS authorization and the
secondary escrow flow were cross-checked against
[ICPS](https://github.com/steilerDev/icloud-photos-sync); no ICPS implementation
code is included. ADP and web-access settings are never changed by this application.
Protocol fixtures are distinct from real account compatibility evidence.

The bundled `crates/wallpapers/assets/seeta_fd_frontal_v1.0.bin` is the
SeetaFace detection model distributed with rustface0.1.7 (BSD2-Clause;
`SeetaFace-BSD-2-Clause.txt`). SHA-256:
`c4619d066ed35e84d9a8e842860b0dff567aba0cbb139881075538761db3ff5d`.
Rustface is compiled without its default Rayon feature to bound preparation
concurrency. Linux HEIF decoding links the installed libheif through libheif-rs;
no Python or Node decoder is invoked. moxcms performs ICC conversion to sRGB.

Solar events use the pinned `sunrise3.0.0` Rust crate by Nathan Osman,
MIT licensed (`sunrise-MIT.txt`). The theme generator is the separately installed
Matugen executable (tested4.2.0, GPL2.0-only); no Matugen implementation code is
copied into or linked into this crate. Aether4.32.0 was evaluated separately.
See `docs/USAGE.md` for the supported styles and limitations.
