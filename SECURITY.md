# Security reports

Do not post passwords, verification codes, session cookies, helper tokens,
private media, catalogs, coordinates or exploit details in public issues.

Use GitHub private vulnerability reporting on this repository's Security tab.
If that feature is unavailable, contact the owner through a verified private
channel before sharing sensitive details. Publication preparation includes
enabling private vulnerability reporting before the public launch.

Reports should identify the version/commit, affected component and a synthetic
reproduction. Ordinary support reports should contain sanitized error codes
and environment versions only. Review status JSON locally before sharing it:
it can contain photo paths, source counts and an estimated location label.

The QML plugin runs with the user's privileges. A separate same-user Rust
service performs network and image work. Apple passwords/MFA codes are not
persisted; required session cookies and optional helper credentials are stored
privately with restrictive permissions. Neither should be placed in shell.json,
command arguments, screenshots or issue reports. Shared accounts and a
compromised desktop remain outside the isolation provided by a same-user socket.

See [release review scope and limitations](docs/SECURITY_REVIEW.md). Exact
source/artifact identities and dated checks belong to the release's audit evidence.
