# Release security review scope

Release evidence identifies the exact source commit, executable and archive
hashes, scanner versions and advisory dates. Review is performed by the same
author who implemented the changes; it is not an independent external audit.
Marketplace approval is a separate listing decision.

The review covers the Rust Apple client, downloads, cache and image pipeline;
same-user Unix IPC and native QML inputs; optional Mac metadata and legacy media
paths; optional model/runtime downloads and native inference; durable privacy
surfaces and admission/correction caches; installers, rollback, packaging,
licenses and public promotional assets.
Synthetic adversarial fixtures exercise unsafe filesystem objects, corrupted
state, fragmented/stalled IPC, metadata limits and image resource limits.
Real acceptance must separately verify installation, native settings, offline
rotation, fresh normal Apple authentication/download and private Mac enrichment.
Fixture or reused-session results do not establish those interactions.

The desktop user and administrator are trusted. QML and the Rust service run
with that user's privileges; a same-user socket cannot isolate a compromised
desktop. Private files reject unsafe ownership, writable ancestors and user
symlink paths. Root-owned operating-system aliases are supported. There is no
promise of forensic erasure of credentials from GUI/runtime memory. Passwords
and verification codes are not persisted; authenticated session material is.

Linux HEIF requires system libheif >=1.23.7, checked before parsing. Explicit
allocation, image, tile and metadata budgets limit decoding. Upstream advisory
triage distinguishes the primary-image decoding API used here from uncalled
region-mask creation, item-data enumeration and standalone TIFF CLI helpers;
it does not assert that every API in the system library is vulnerability-free.
Keep system libraries updated. The pinned Mac pillow-heif wheel bundles an
older libheif and its legacy HEIF path fails closed. Default Mac metadata-only
operation does not export or decode images. Legacy media dispatch is restricted
to JPEG, PNG and version-gated HEIF decoders; unexpected formats cannot invoke
external image renderers. See [Mac setup](MAC_SETUP.md).

Optional screening downloads use pinned sources, byte counts and SHA-256.
Staged extraction reads only the named runtime library; models are loaded from
verified bytes in a bounded subprocess. Those controls do not establish the
absence of unknown native-runtime defects. Screening accuracy has substantial
limits and is separate from code-security review; see
[the evaluation](PRIVACY_MODEL_EVALUATION.md). A confirmed desktop surface is
required before Active. Supported lifecycle operations serialize against mode
changes and respect the durable protection flag, including a failed settings
commit. Invalid flags fail closed until an explicit privacy-off repair.
Disabling the plugin or quitting the shell removes its native protective surface.

The helper accepts no arbitrary SQL. Photos access uses fixed read-only queries
against consistent snapshots; app catalog writes are parameterized. Python's
bundled SQLite and native codec reachability are reviewed separately from
Python package advisories. A clean Cargo or pip advisory scan alone does not
cover interpreters, native libraries, the entire host, or unknown defects.

Authentication uses Apple's undocumented consumer web interfaces and can break
when Apple changes them. Normal MFA, device approval and macOS privacy prompts
remain required. The optional helper uses authenticated private Tailscale HTTPS,
never public Funnel. Private media, catalogs, tokens and QA captures are excluded
from the public source and history. Promotional people and device scenes are
AI-generated; screen captures and composites are identified in their provenance.

Report vulnerabilities privately using the instructions in [SECURITY.md](../SECURITY.md).
