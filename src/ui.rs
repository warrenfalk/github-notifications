use gh_comments::{
    open_github,
    store::{Item, Store},
};
use iced::{
    Element, Subscription, Task,
    widget::{button, column, container, row, scrollable, text},
};
use std::{path::PathBuf, time::Duration};

pub fn run(path: PathBuf) -> anyhow::Result<()> {
    iced::application(
        move || {
            let state = Inbox {
                path: path.clone(),
                items: Vec::new(),
                status: "Loading local inbox…".into(),
                loading: true,
                revision: 0,
            };
            let task = load(state.path.clone(), state.revision);
            (state, task)
        },
        Inbox::update,
        Inbox::view,
    )
    .title("GitHub comments")
    .subscription(Inbox::subscription)
    .window_size((900.0, 700.0))
    .run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_refresh_cannot_restore_a_dismissed_comment() {
        let item = Item {
            id: "review:1".into(),
            title: "PR".into(),
            repository: "a/b".into(),
            author: "someone".into(),
            body: "reply".into(),
            url: "https://github.com/a/b/pull/1".into(),
            reason: "Reply".into(),
            created_at: "2026-10-28T00:00:00Z".into(),
            read: false,
        };
        let mut inbox = Inbox {
            path: PathBuf::from("unused.sqlite3"),
            items: vec![item.clone()],
            status: "Synced".into(),
            loading: true,
            revision: 0,
        };
        let _ = inbox.update(Message::Changed(Ok(vec![item.id.clone()])));
        assert!(inbox.items.is_empty());
        let _ = inbox.update(Message::Loaded(0, Ok((vec![item], "Old snapshot".into()))));
        assert!(inbox.items.is_empty());
        assert!(inbox.loading);
        let _ = inbox.update(Message::Loaded(1, Ok((vec![], "Current snapshot".into()))));
        assert!(!inbox.loading);
        assert_eq!(inbox.status, "Current snapshot");
    }
}

struct Inbox {
    path: PathBuf,
    items: Vec<Item>,
    status: String,
    loading: bool,
    revision: u64,
}

#[derive(Clone, Debug)]
enum Message {
    Refresh,
    Loaded(u64, Result<(Vec<Item>, String), String>),
    Open(String, String),
    Dismiss(Vec<String>),
    Changed(Result<Vec<String>, String>),
}

fn load(path: PathBuf, revision: u64) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                let store = Store::open(&path)?;
                let status = format!(
                    "Last sync: {} | Error: {} | Coverage: {}",
                    store
                        .meta("last_success")?
                        .unwrap_or_else(|| "never; start `gh-comments watch`".into()),
                    store.meta("last_error")?.unwrap_or_else(|| "none".into()),
                    store
                        .meta("coverage_warning")?
                        .unwrap_or_else(|| "PRs and issues only".into())
                );
                Ok((store.list(false)?, status))
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
        },
        move |result| Message::Loaded(revision, result),
    )
}

impl Inbox {
    fn refresh(&mut self) -> Task<Message> {
        if self.loading {
            return Task::none();
        }
        self.loading = true;
        load(self.path.clone(), self.revision)
    }

    fn subscription(&self) -> Subscription<Message> {
        // The collector is a separate process. Refresh its committed local
        // database without increasing the frequency of GitHub API requests.
        iced::time::every(Duration::from_secs(1)).map(|_| Message::Refresh)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => return self.refresh(),
            Message::Loaded(revision, result) => {
                self.loading = false;
                // A read started before a dismissal or mark-read operation may
                // contain stale rows. Never put them back into the visible list.
                if revision != self.revision {
                    return self.refresh();
                }
                match result {
                    Ok((items, status)) => {
                        if self.items != items {
                            self.items = items;
                        }
                        if self.status != status {
                            self.status = status;
                        }
                    }
                    Err(error) => self.status = error,
                }
            }
            Message::Open(id, url) => {
                let path = self.path.clone();
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
                            open_github(&url)?;
                            Store::open(&path)?.mark_read(&id)?;
                            Ok(Vec::new())
                        })
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())
                    },
                    Message::Changed,
                );
            }
            Message::Dismiss(ids) => {
                let path = self.path.clone();
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
                            Store::open(&path)?.dismiss(&ids)?;
                            Ok(ids)
                        })
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())
                    },
                    Message::Changed,
                );
            }
            Message::Changed(Ok(dismissed)) => {
                self.items.retain(|item| !dismissed.contains(&item.id));
                self.revision += 1;
                return self.refresh();
            }
            Message::Changed(Err(error)) => self.status = error,
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let mut list = column![].spacing(12);
        for item in &self.items {
            let excerpt: String = item.body.chars().take(400).collect();
            list = list.push(
                container(
                    column![
                        text(format!(
                            "{}{} · @{}",
                            if item.read { "" } else { "● " },
                            item.repository,
                            item.author
                        ))
                        .size(16),
                        text(&item.title).size(20),
                        text(format!("{} · {}", item.reason, item.created_at)).size(14),
                        text(excerpt),
                        row![
                            button("Open on GitHub")
                                .on_press(Message::Open(item.id.clone(), item.url.clone())),
                            button("Dismiss").on_press(Message::Dismiss(vec![item.id.clone()])),
                        ]
                        .spacing(8),
                    ]
                    .spacing(6),
                )
                .padding(12)
                .style(container::rounded_box),
            );
        }
        if self.items.is_empty() {
            list = list.push(text("No relevant comments collected yet."));
        }
        container(
            column![
                row![
                    text("GitHub comments").size(28),
                    button("Refresh local inbox").on_press(Message::Refresh),
                    button("Dismiss all shown").on_press_maybe((!self.items.is_empty()).then(
                        || Message::Dismiss(
                            self.items.iter().map(|item| item.id.clone()).collect()
                        )
                    )),
                ]
                .spacing(20),
                text(&self.status).size(13),
                scrollable(list),
            ]
            .spacing(16),
        )
        .padding(20)
        .into()
    }
}
