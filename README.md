# github-notifications

A local GitHub notification inbox and Linux desktop notifier. The first implementation
filters comments on changed pull requests and issues rather than forwarding every
GitHub notification.

## Development

The Nix flake supplies Rust, Cargo, rustfmt, Clippy, rust-analyzer, GitHub CLI,
SQLite, notification tools, and the windowing libraries used by Iced. Both the
Nix input and Cargo dependencies are locked.

```sh
direnv allow
cargo test
cargo clippy --all-features --all-targets -- -D warnings
cargo build --features gui
```

Without direnv, prefix commands with `nix develop -c`.

## Nix package and autostart

The default package includes the collector, Iced inbox, runtime windowing
libraries, browser-opening support, and a **GitHub Notifications** desktop launcher.
The build source contains only Cargo manifests and Rust sources; local credential
files, databases, and build artifacts are excluded.

```sh
nix build .
nix run . -- inbox
nix run . -- sync
```

For NixOS, add this repository as a flake input:

```nix
inputs.github-notifications = {
  url = "github:warrenfalk/github-notifications";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Import `inputs.github-notifications.nixosModules.default` in the host's modules
and enable it for your desktop user:

```nix
services.github-notifications = {
  enable = true;
  user = "warren";
};
```

The module installs the package and starts `github-notifications.service` as a
user service with `graphical-session.target`. It stops with the graphical session,
restarts on failure, and opens no inbox window automatically. Launch the inbox
from the application menu or with `github-notifications inbox`.

Credentials remain in the user's runtime config file, outside the Nix store.
Create that saved token interactively before enabling autostart. If your shell
already exports an override, unset it for this command so the prompt saves a file:

```sh
env -u GITHUB_NOTIFICATIONS_TOKEN github-notifications sync
```

Stop any terminal-run collector before starting the service; both use the same
database lock. After activating the NixOS configuration, log into a graphical
session or start the service from your existing session:

```sh
systemctl --user start github-notifications
systemctl --user status github-notifications
journalctl --user -u github-notifications -n 50
```

## Usage

Collector authentication uses the first available source:

1. A nonempty `GITHUB_NOTIFICATIONS_TOKEN` environment variable.
2. The `token` field in `$XDG_CONFIG_HOME/github-notifications/github-notifications.toml`, normally
   `~/.config/github-notifications/github-notifications.toml`.
3. An interactive terminal prompt. Input is hidden, and the token is saved to the
   config file for subsequent runs, including a future user service.

```toml
token = "ghp_your_classic_personal_access_token"
```

Run `cargo run -- sync` in a terminal for first-time setup. The app creates its
config directory with mode `0700` and writes the config atomically with mode
`0600` on Unix. Tokens supplied through the environment are not saved. Existing
unrelated TOML settings are preserved. Edit the config's `token` field to replace
a saved token.

Non-interactive runs never prompt: they require the environment variable or a
saved token and otherwise exit with setup instructions. Local commands (`inbox`,
`list`, `status`, and `open`) do not need a token. `GH_TOKEN` and GitHub CLI login
are not used for authentication. An ignored `.env.local` can still hold
`GITHUB_NOTIFICATIONS_TOKEN` for direnv to load.

Use a classic personal access token: fine-grained PATs cannot access GitHub's
notifications endpoint. The API requires the `notifications` or `repo` scope, and
fetching private repository comments requires `repo`. Organization SSO restrictions
may require additional authorization. Tokens are not stored in the inbox database.

```sh
# Fetch recent activity once. Initial import adds history without desktop alerts.
cargo run -- sync

# Keep collecting and send native desktop notifications.
cargo run -- watch

# Inspect the local inbox and collector status without contacting GitHub.
cargo run -- list
cargo run -- status

# Open the optional Iced inbox in another terminal while watch runs.
cargo run --features gui -- inbox

# Open an item using the ID shown by list.
cargo run -- open review:12345
```

Desktop notifications request no expiration and open the matching comment on
GitHub. The desktop notification server ultimately controls their presentation.
Linux notification actions require support from the notification server, and the collector
must remain running to handle clicks. The first inbox UI provides excerpts, local
read state, automatic refresh within about one second, a manual refresh button,
and links to GitHub. Automatic refresh reads only the local database and does not
make extra GitHub requests. The inbox does not start the collector itself; close
the window without stopping a separate `watch` process or enabled user service.
Autostart is opt-in through the NixOS module above.

Use **Dismiss** to remove a comment from the local list, or **Dismiss all shown**
to clear the currently displayed comments. Dismissal preserves each comment's ID
for deduplication, so a later sync will not bring it back. New replies have their
own IDs and still appear. Dismissal marks pending desktop delivery as handled;
alerts already being delivered or displayed are not recalled. It does not change
GitHub's notification/subscription state.

Automatic refresh runs a SQLite read on a background worker once per second.
It keeps the current list visible while loading, ignores unchanged results, and
does not toggle the Refresh button's appearance. A read started before a local
dismissal cannot restore the dismissed rows when it finishes.

The default database is `$XDG_DATA_HOME/github-notifications/inbox.sqlite3`, usually
`~/.local/share/github-notifications/inbox.sqlite3`. Override it with `--database PATH`.
Private comment text is stored locally; the database file is restricted to its
owner on Unix. Local read state is independent of GitHub read state. Opening a
comment in the browser can affect GitHub's read state normally.

## Relevance rules

- New comments from other people that mention your username.
- New review comments in the same review conversation where you previously
  commented, even if the reply does not mention you.
- Submitted reviews on PRs you authored: approvals, change requests, and
  comment-only reviews, including reviews with an empty summary body.
- Direct requests naming your account as a reviewer. These use dated timeline
  events, so removing and re-requesting your review creates a new notification;
  unrelated later PR activity does not repeat the original request.
- Your own comments are excluded. A different conversation on the same PR does
  not qualify merely because you participated elsewhere on that PR.

Mention matching checks rendered Markdown text with username boundaries and
ignores inline/fenced code. It is a heuristic rather than GitHub's exact mention
parser; quoted mentions and escaped mentions are not distinguished yet.

## Collection and reliability

GitHub notifications identify changed subjects; their reasons are not used as
per-comment event classifications. Collection includes already-read notifications,
follows pagination, and overlaps the persisted checkpoint by five minutes.
Successfully processed subjects are cached by update timestamp. Unchanged feed
requests use GitHub's conditional polling support. The default minimum polling
interval is 60 seconds; server polling and retry headers can lengthen it.

Inbox writes, processed-subject timestamps, and the checkpoint commit together.
A failed subject causes rollback and retry rather than silently advancing past it.
Only one collector can run per database. This also means an inaccessible subject
currently blocks that batch; status reports the failure.

Accepted comments are deduplicated by comment type and ID. Initial history is
imported quietly. Later, unseen comments older than the initial import are also
quiet, avoiding historical alert bursts when an old PR becomes active again.
Desktop delivery is tracked separately so failed sends can be retried. A crash
between sending an alert and recording delivery can still produce a duplicate.

On upgrade to review/request support, newly discovered review history at or before
the collector's previous checkpoint is imported quietly. Existing review-summary
IDs, read state, and dismissals are preserved. Review requests link to the PR;
submitted reviews link to the review itself. An alert records the request event,
even if that request has since been removed or fulfilled.

## Coverage and next work

Implemented: PR descriptions, issue descriptions, top-level issue/PR comments,
review summaries, and inline review comments. Descriptions and review summaries
on other people's PRs qualify through mentions only; submitted reviews on your
own PRs also qualify by their review state. Edits and review dismissals are not
treated as new review submissions. Draft/pending reviews are excluded.

Not implemented: Discussions, commit comments, team mentions or team review
requests, conversation grouping/muting, or webhook ingestion. Unsupported feed
types are recorded in collector status. Coverage depends on GitHub emitting a
notification and the account's subscription settings; this is not an independent
repository activity audit. The initial lookback is 30 days of notification
activity (`sync --days N` can change it before the first successful sync).

See [push mechanism research](docs/push-mechanisms.md) for why polling is the
baseline and where repository webhooks could help.
