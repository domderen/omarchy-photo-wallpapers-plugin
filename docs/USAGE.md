# Usage, privacy and limitations

Save/Cancel apply to the source, interval, enhancement and theme draft. Helper
pairing and per-photo day/night feedback save immediately. Meeting On/Off,
model installation and privacy corrections also take effect immediately;
the Personal filter follows Save/Cancel. Read [Meeting Mode](MEETING_MODE.md)
before using it for screen sharing. Stable source IDs
keep equal names distinct; missing sources stay unavailable instead of silently
selecting the whole library. All selected source kinds combine by union.

Next unpins and chooses another prepared image; Previous follows retained
history. Pause/Pin suppress timed photo changes. Auto themes can still change
while the photo is paused/pinned. Day/Night/Either overrides save per photo;
Automatic uses cached brightness/highlight measurements, not scene recognition.
Preferences remain soft and never expand selected sources or undo exclusions.

Themes use the public Omarchy preserve-background API and installed application
templates/hooks. Neutral retains the original surface mapping; Subtle keeps
surfaces restrained, Tinted adds photo colour and Rich increases that colour.
A manual theme selection suspends matching. Toggle matching off/on to resume;
disabling restores the preceding theme only while the app still owns it.
Arbitrary applications and custom browser profiles may need their own theme
refresh/restart. Existing kept QML can require `omarchy-restart-shell` in an
unlocked desktop after an update.

Auto calculates apparent sunrise/sunset locally, with bounded minute offsets.
Current area reuses Omarchy's weather configuration or its existing wttr.in
estimate; that network request reveals normal connection metadata to that
provider. It is an estimate, not GPS. A fixed location avoids automatic lookup.
A private cache supports outages. The UI reports provider/freshness. Location
is not inferred from photos. Solar changes retint the current photo; normal
rotation progressively changes the photo mix.

Apple access uses undocumented consumer iCloud web/CloudKit endpoints, not a
supported public third-party Photos API. Apple can change compatibility or
require normal trusted-device/web account authorization. The app does not
disable Advanced Data Protection or change account security settings. Do not
change them merely to accommodate the app. A successful test on one account is
not proof for every account/security mode. Passwords and verification codes are
not persisted; session cookies are retained privately for renewal.

Hidden/deleted assets are excluded. Personal and supported owned Shared Photo
Library zones are distinct from classic Shared Albums; do not assume every
shared collection is available. Mac enrichment attaches only exact,
account-scoped unambiguous cloud-master matches. Unmatched/ambiguous images
receive no guessed identity. Named People require imported membership; generic
face detection does not identify relatives. Imported named face geometry is
currently unavailable, so conservative full-frame fallback protects subjects.

Portrait/group fallbacks can use a smaller full image over a blurred backdrop.
A detector can miss small faces; good framing is not guaranteed. Originals and
person assignments are never modified. Enhancements apply only to prepared
copies. Direct edited/original rendition availability is explicit. JPEG,
PNG, WebP, TIFF and native Linux HEIF are supported. PQ/HLG-only HDR HEIF is
rejected until a verified tone-mapping path exists; Apple JPEG is preferred.
The installed public background API applies one image to all displays using
the largest active output's geometry; per-output backgrounds are unverified.

The app keeps bounded local caches and one preparation worker; ready changes
avoid analysis. Internet outages retain usable cached photos. An offline Mac
retains last imported People membership, marked stale, while Apple downloads
can continue. A new installation with no imported People data offers cloud
sources instead. It never guesses missing named membership.

Private state uses standard application config/data/runtime directories.
The IPC socket and session/helper files use restrictive permissions. Credentials
never belong in shell.json, process arguments, Git or support screenshots.
`status --json` can still expose media paths and an estimated location label;
inspect and redact it before sharing. Use sanitized error codes for reports.

Service operations: `systemctl --user start|stop|restart omarchy-photo-wallpapers`.
For unavailable service guidance, check installation, current desktop-user
systemd environment and `journalctl --user -u omarchy-photo-wallpapers` locally.
Do not attach raw private journals/catalogs. See SECURITY.md for private reports.
