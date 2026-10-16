# Security

Ambient handles conversations and transcripts. Please avoid posting sensitive
recordings, transcript excerpts, credentials or full home-directory paths in a
public issue or pull request.

For a vulnerability, use GitHub's **Report a vulnerability** option in the
repository's Security tab when available. If private reporting is unavailable,
open an issue requesting a private reporting channel without exploit details or
sensitive data. Include reproduction details only once a private channel exists.

Describe the affected version, impact and a minimal reproduction using synthetic
data. There is no guaranteed response time or supported-version window; Ambient
is early software, and fixes target the current development version.

## Data boundary

Capture, transcription and speaker separation run locally. Sessions are ordinary
files protected by your macOS account and storage settings, not an encrypted
application vault. Cloud folder sync and backups can copy them elsewhere.

An MCP client can read sessions available to the Ambient process. Connecting an
assistant may send those contents to its provider; a read-only tool still grants
access to conversation text. Exporting and copying transcripts have the same
sharing implications.

See [what is kept](docs/using/what-is-kept.md) for consent and retention behavior,
including audio retained after a failed transcription.
