# Attribution and distribution

Original application code is offered under the MIT license. Third-party code,
model data and system components retain their own licenses. See
`third_party/README.md`, its individual notices and `third_party/rust/` for the
locked Rust dependency inventory and available upstream license texts.
`third_party/rust-stdlib/` retains the exact build toolchain's standard-library
notice and bundled library-component licenses, separately from Cargo packages.

The included SeetaFace detector is BSD-2-Clause licensed. Adapted Apple SRP
arithmetic from kei is MIT licensed and attributed at its pinned revision.
Solar calculations use the MIT-licensed sunrise crate. Optional screening uses
the Apache-2.0 models and MIT ONNX Runtime described in
`third_party/privacy-model-NOTICE.md`; their license texts and native third-party
notices are included. No model weights or runtime library are bundled. The Rust dependency
inventory includes development and target-specific packages as a conservative
superset; not all entries are linked into the Linux executable.

Linux builds dynamically link the separately installed libheif, licensed
LGPL-3.0-or-later. This release does not distribute libheif or its codec
binaries. Users retain the ability to replace compatible system libraries.
Library source is available from https://github.com/strukturag/libheif;
Arch package source and modifications are at
https://gitlab.archlinux.org/archlinux/packaging/packages/libheif.
Matugen (GPL-2.0-only) is a separate optional executable, not bundled or linked
into this program. Python helper dependencies are separately installed by uv;
their original licenses apply.

Sample family photography and the device/background scene in assets/promo are
AI-generated, featuring fictional people. Real Omarchy captures are identified
in their provenance. Device composites are illustrations, not photographs of
a physical installation. The project makes no claim of copyright over purely
AI-generated material where such rights do not arise; to the extent the author
holds rights in original promotional assets, their use is permitted under the
project MIT license. Third-party trademarks remain their owners' property.

This project is independent and is not affiliated with or endorsed by Apple,
Omarchy or Matugen. Apple/MacBook and other names identify compatibility or
illustrated hardware. Apple account access uses undocumented consumer web
interfaces and may stop working when those interfaces change.
