use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use github_notifications::{database_path, filter, github::GitHub, open_github, store::Store};
use std::{fs::OpenOptions, path::PathBuf, time::Duration as StdDuration};

#[cfg(feature = "gui")]
mod ui;

#[derive(Parser)]
#[command(
    version,
    about = "A local GitHub notification inbox and desktop notifier"
)]
struct Cli {
    /// Override the local SQLite inbox path.
    #[arg(long, global = true)]
    database: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch relevant comments once. First sync imports quietly.
    Sync {
        /// Initial notification lookback; ignored after the first successful sync.
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// Collect continuously and send clickable desktop alerts.
    Watch {
        /// Minimum seconds between polls; GitHub can require a longer interval.
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
    },
    /// List the persistent local inbox without contacting GitHub.
    List,
    /// Open an inbox item's exact GitHub URL and mark it read locally.
    Open { id: String },
    /// Show collector status without contacting GitHub.
    Status,
    /// Open the local desktop inbox (requires --features gui).
    Inbox,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.database.map(Ok).unwrap_or_else(database_path)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&path)?;
    match cli.command {
        Command::List => {
            for item in store.list(false)? {
                println!(
                    "{} {} | {} | @{} | {}\n  {}\n  {}",
                    if item.read { " " } else { "*" },
                    item.id,
                    item.repository,
                    item.author,
                    item.reason,
                    item.title,
                    item.url
                );
            }
            Ok(())
        }
        Command::Open { id } => {
            let item = store
                .list(false)?
                .into_iter()
                .find(|item| item.id == id)
                .context("Unknown inbox item ID")?;
            open_github(&item.url)?;
            store.mark_read(&id)
        }
        Command::Status => {
            for key in ["account", "last_success", "last_error", "coverage_warning"] {
                println!("{key}: {}", store.meta(key)?.as_deref().unwrap_or("none"));
            }
            Ok(())
        }
        Command::Inbox => {
            #[cfg(feature = "gui")]
            {
                ui::run(path)
            }
            #[cfg(not(feature = "gui"))]
            {
                anyhow::bail!("Build the inbox with `cargo run --features gui -- inbox`")
            }
        }
        command => {
            // Only one collector may own the sync checkpoint and alert outbox.
            let lock_path = path.with_extension("collector.lock");
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(lock_path)?;
            fs2::FileExt::try_lock_exclusive(&lock)
                .context("Another collector is already running for this database")?;
            tokio::runtime::Runtime::new()?.block_on(async move {
                let mut github = GitHub::connect().await?;
                let username = github.username().await?;
                store.check_account(&username)?;
                match command {
                    Command::Sync { days } => {
                        let result = sync(&mut github, &store, &username, days).await;
                        if let Err(error) = &result {
                            store.set_meta(
                                "last_error",
                                &format!("{}: {error:#}", Utc::now().to_rfc3339()),
                            )?;
                        }
                        result
                    }
                    Command::Watch { interval } => {
                        let mut failures = 0u32;
                        loop {
                            let result = tokio::select! {
                                result = sync(&mut github, &store, &username, 30) => result,
                                _ = tokio::signal::ctrl_c() => return Ok(()),
                            };
                            match result {
                                Ok(()) => {
                                    failures = 0;
                                }
                                Err(error) => {
                                    failures = failures.saturating_add(1);
                                    eprintln!("Sync failed: {error:#}");
                                    store.set_meta(
                                        "last_error",
                                        &format!("{}: {error:#}", Utc::now().to_rfc3339()),
                                    )?;
                                }
                            }
                            deliver(&store)?;
                            let backoff = if failures == 0 {
                                0
                            } else {
                                60u64.saturating_mul(1 << failures.min(6))
                            };
                            let delay = interval.max(github.poll_interval).max(backoff);
                            tokio::select! {
                                _ = tokio::time::sleep(StdDuration::from_secs(delay)) => {},
                                _ = tokio::signal::ctrl_c() => return Ok(()),
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            })
        }
    }
}

async fn sync(github: &mut GitHub, store: &Store, username: &str, days: u32) -> Result<()> {
    let now = Utc::now();
    let initialized = store.meta("initialized_at")?;
    let initial = initialized.is_none();
    let initialized_at = initialized.map(|s| s.parse()).transpose()?.unwrap_or(now);
    let since = store
        .checkpoint()?
        .unwrap_or(now - Duration::days(i64::from(days)));
    // Newly supported review events discovered during upgrade may include old
    // history. Import it quietly, while alerting events newer than our last
    // checkpoint. Keep this baseline stable across subsequent polls/restarts.
    let review_activity_since: chrono::DateTime<Utc> = store
        .meta("review_activity_since")?
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(since);
    let batch = github.notifications(since).await?;
    let transaction = store.0.unchecked_transaction()?;
    let mut checkpoint = since;
    let mut unsupported = std::collections::BTreeSet::new();
    let before = store.list(false)?.len();
    for notification in batch.notifications {
        checkpoint = checkpoint.max(notification.updated_at);
        if !matches!(notification.subject.kind.as_str(), "PullRequest" | "Issue") {
            unsupported.insert(notification.subject.kind);
            continue;
        }
        let subject_key = format!(
            "subject:v2:{}",
            notification
                .subject
                .url
                .as_deref()
                .context("Notification has no subject URL")?
        );
        let updated_at = notification.updated_at.to_rfc3339();
        if store.meta(&subject_key)?.as_deref() == Some(&updated_at) {
            continue;
        }
        let conversation = github
            .comments(&notification.subject)
            .await
            .with_context(|| {
                format!(
                    "Processing {}: {}",
                    notification.repository.full_name, notification.subject.title
                )
            })?;
        for (comment, reason) in filter::relevant(conversation, username) {
            let review_activity =
                comment.id.starts_with("review-body:") || comment.id.starts_with("review-request:");
            store.insert(
                &comment,
                &notification.subject.title,
                &notification.repository.full_name,
                reason,
                initial
                    || comment.created_at <= initialized_at
                    || (review_activity && comment.created_at <= review_activity_since),
            )?;
        }
        store.set_meta(&subject_key, &updated_at)?;
    }
    if initial {
        store.set_meta("initialized_at", &now.to_rfc3339())?;
    }
    store.set_meta("checkpoint", &checkpoint.to_rfc3339())?;
    store.set_meta("review_activity_since", &review_activity_since.to_rfc3339())?;
    store.set_meta("last_success", &now.to_rfc3339())?;
    store.set_meta("last_error", "none")?;
    let encountered_unsupported = !unsupported.is_empty();
    let warning = if unsupported.is_empty() {
        store.meta("coverage_warning")?.unwrap_or_else(|| {
            "PRs and issues only; Discussions and commit comments are not implemented".into()
        })
    } else {
        format!(
            "PRs and issues only. Unsupported notification types encountered: {}",
            unsupported.into_iter().collect::<Vec<_>>().join(", ")
        )
    };
    store.set_meta("coverage_warning", &warning)?;
    let added = store.list(false)?.len() - before;
    transaction.commit()?;
    github.accept(batch.validator);
    println!(
        "{}: {added} relevant item(s) added{}",
        now.to_rfc3339(),
        if initial {
            " (quiet initial import)"
        } else {
            ""
        }
    );
    if encountered_unsupported {
        eprintln!("{warning}");
    }
    Ok(())
}

fn deliver(store: &Store) -> Result<()> {
    for item in store.list(true)? {
        let excerpt: String = item.body.chars().take(240).collect();
        // Escape server notification markup; remote comment text is untrusted.
        let body = format!("{}\n{}", item.reason, excerpt)
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        match notify_rust::Notification::new()
            .appname("github-notifications")
            .summary(&format!("@{} · {}", item.author, item.repository))
            .body(&body)
            .action("default", "Open on GitHub")
            .timeout(notify_rust::Timeout::Never)
            .show()
        {
            Ok(handle) => {
                store.delivered(&item.id)?;
                std::thread::spawn(move || {
                    handle.wait_for_action(|action| {
                        if action == "default"
                            && let Err(error) = open_github(&item.url)
                        {
                            eprintln!("Could not open GitHub: {error:#}");
                        }
                    });
                });
            }
            Err(error) => {
                // Keep the outbox entry pending. The inbox remains available.
                store.set_meta(
                    "last_error",
                    &format!("Desktop notification failed: {error}"),
                )?;
                eprintln!("Desktop notification failed: {error}");
                break;
            }
        }
    }
    Ok(())
}
