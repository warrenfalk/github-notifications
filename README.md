# gh-comments

A local GitHub comment inbox and Linux desktop notifier. The first implementation
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

## Usage

Authenticate with GitHub CLI (`gh auth login`), or supply a classic personal access
token in `GH_COMMENTS_TOKEN` (preferred for app-specific credentials) or `GH_TOKEN`.
An ignored `.env.local` can hold `GH_COMMENTS_TOKEN` for direnv to load. Fine-grained
PATs cannot access GitHub's notifications endpoint. The API requires the `notifications` or `repo`
scope, and fetching private repository comments also requires repository access.
Organization SSO or app restrictions may require additional authorization. Tokens
are read at startup and are not stored in the inbox database.

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
the window without stopping a separate `watch` process.
No autostart service is installed.

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

The default database is `$XDG_DATA_HOME/gh-comments/inbox.sqlite3`, usually
`~/.local/share/gh-comments/inbox.sqlite3`. Override it with `--database PATH`.
Private comment text is stored locally; the database file is restricted to its
owner on Unix. Local read state is independent of GitHub read state. Opening a
comment in the browser can affect GitHub's read state normally.

## Relevance rules

- New comments from other people that mention your username.
- New review comments in the same review conversation where you previously
  commented, even if the reply does not mention you.
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

## Coverage and next work

Implemented: PR descriptions, issue descriptions, top-level issue/PR comments,
review summaries, and inline review comments. Descriptions and review summaries
currently qualify through mentions only. Edits are not treated as new events.

Not implemented: Discussions, commit comments, team mentions, conversation
grouping/muting, or webhook ingestion. Unsupported feed
types are recorded in collector status. Coverage depends on GitHub emitting a
notification and the account's subscription settings; this is not an independent
repository activity audit. The initial lookback is 30 days of notification
activity (`sync --days N` can change it before the first successful sync).

See [push mechanism research](docs/push-mechanisms.md) for why polling is the
baseline and where repository webhooks could help.
