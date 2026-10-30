use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Duration, Utc};
use reqwest::{Client, Response, StatusCode, header};
use serde::{Deserialize, de::DeserializeOwned};
use std::time::Duration as StdDuration;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct User {
    pub login: String,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: String,
    pub user: User,
    pub body: String,
    pub html_url: String,
    pub created_at: DateTime<Utc>,
    pub thread_id: Option<u64>,
}

#[derive(Deserialize)]
pub struct Notification {
    pub subject: Subject,
    pub updated_at: DateTime<Utc>,
    pub repository: Repository,
}

#[derive(Deserialize)]
pub struct Repository {
    pub full_name: String,
}

#[derive(Deserialize)]
pub struct Subject {
    pub title: String,
    pub url: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize)]
struct RawComment {
    id: u64,
    user: Option<User>,
    body: Option<String>,
    html_url: String,
    created_at: DateTime<Utc>,
    in_reply_to_id: Option<u64>,
}

impl RawComment {
    fn into_comment(self, kind: &str, review: bool) -> Comment {
        Comment {
            id: format!("{kind}:{}", self.id),
            user: self.user.unwrap_or_default(),
            body: self.body.unwrap_or_default(),
            html_url: self.html_url,
            created_at: self.created_at,
            thread_id: review.then_some(self.in_reply_to_id.unwrap_or(self.id)),
        }
    }
}

#[derive(Deserialize)]
struct Review {
    id: u64,
    user: Option<User>,
    body: Option<String>,
    html_url: String,
    submitted_at: Option<DateTime<Utc>>,
    state: ReviewState,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
    #[serde(other)]
    Other,
}

impl Review {
    fn into_comment(self) -> Option<(Comment, ReviewState)> {
        if self.state == ReviewState::Pending {
            return None;
        }
        Some((
            Comment {
                // Preserve the existing ID so mentions previously accepted from a
                // review summary do not become duplicate alerts after this change.
                id: format!("review-body:{}", self.id),
                user: self.user.unwrap_or_default(),
                body: self.body.unwrap_or_default(),
                html_url: self.html_url,
                created_at: self.submitted_at?,
                thread_id: None,
            },
            self.state,
        ))
    }
}

#[derive(Deserialize)]
#[serde(tag = "event")]
enum TimelineEvent {
    #[serde(rename = "review_requested")]
    ReviewRequested {
        id: u64,
        actor: Option<User>,
        created_at: DateTime<Utc>,
        requested_reviewer: Option<User>,
    },
    #[serde(other)]
    Other,
}

pub struct ReviewRequest {
    pub comment: Comment,
    pub reviewer: User,
}

impl TimelineEvent {
    fn into_request(self, pull_request_url: &str) -> Option<ReviewRequest> {
        let Self::ReviewRequested {
            id,
            actor,
            created_at,
            requested_reviewer,
        } = self
        else {
            return None;
        };
        Some(ReviewRequest {
            reviewer: requested_reviewer?,
            comment: Comment {
                id: format!("review-request:{id}"),
                user: actor.unwrap_or_default(),
                body: "Requested your review of this pull request.".into(),
                html_url: pull_request_url.to_owned(),
                created_at,
                thread_id: None,
            },
        })
    }
}

#[derive(Default)]
pub struct Conversation {
    pub comments: Vec<Comment>,
    pub review_comments: Vec<Comment>,
    pub reviews: Vec<(Comment, ReviewState)>,
    pub review_requests: Vec<ReviewRequest>,
    pub pull_request_author: Option<User>,
}

pub struct Batch {
    pub notifications: Vec<Notification>,
    pub validator: Option<(String, String)>,
}

pub struct GitHub {
    client: Client,
    validator: Option<(String, String)>,
    pub poll_interval: u64,
}

impl GitHub {
    pub async fn connect() -> Result<Self> {
        let token = tokio::task::spawn_blocking(crate::config::load_token)
            .await
            .context("Token setup worker failed")??;
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {token}"))?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            header::HeaderValue::from_static("2022-11-28"),
        );
        Ok(Self {
            client: Client::builder()
                .user_agent("gh-comments/0.1.0")
                .default_headers(headers)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(StdDuration::from_secs(30))
                .build()?,
            validator: None,
            poll_interval: 60,
        })
    }

    async fn get(&mut self, url: &str, conditional: bool) -> Result<Response> {
        let parsed = reqwest::Url::parse(url)?;
        ensure!(
            parsed.scheme() == "https" && parsed.host_str() == Some("api.github.com"),
            "Unexpected API URL"
        );
        let mut request = self.client.get(parsed);
        if conditional
            && let Some((cached_url, modified)) = &self.validator
            && cached_url == url
        {
            request = request.header(header::IF_MODIFIED_SINCE, modified);
        }
        let response = request.send().await?;
        let headers = response.headers();
        if let Some(interval) = headers
            .get("x-poll-interval")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            self.poll_interval = interval.max(60);
        }
        if let Some(delay) = headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            self.poll_interval = self.poll_interval.max(delay);
        }
        if headers
            .get("x-ratelimit-remaining")
            .is_some_and(|v| v == "0")
            && let Some(reset) = headers
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
        {
            self.poll_interval = self
                .poll_interval
                .max((reset - Utc::now().timestamp()).max(0) as u64 + 1);
        }
        if response.status().is_client_error() || response.status().is_server_error() {
            if response.status() == StatusCode::FORBIDDEN
                && url.starts_with("https://api.github.com/notifications?")
            {
                bail!(
                    "GitHub notifications returned HTTP 403. This endpoint does not support fine-grained PATs. Set GH_COMMENTS_TOKEN to a classic PAT with notifications scope and repository access as needed; also check organization SSO restrictions"
                );
            }
            // Do not print response bodies, headers, or credentials.
            bail!("GitHub returned HTTP {} for {}", response.status(), url);
        }
        Ok(response)
    }

    async fn list<T: DeserializeOwned>(&mut self, base: &str) -> Result<Vec<T>> {
        let mut result = Vec::new();
        for page in 1.. {
            let url = format!("{base}?per_page=100&page={page}");
            let response = self.get(&url, false).await?;
            let more = has_next(&response);
            result.extend(response.json::<Vec<T>>().await?);
            if !more {
                break;
            }
        }
        Ok(result)
    }

    pub async fn username(&mut self) -> Result<String> {
        Ok(self
            .get("https://api.github.com/user", false)
            .await?
            .json::<User>()
            .await?
            .login)
    }

    pub async fn notifications(&mut self, since: DateTime<Utc>) -> Result<Batch> {
        let mut notifications = Vec::new();
        let mut validator = None;
        for page in 1.. {
            let mut url = reqwest::Url::parse("https://api.github.com/notifications")?;
            url.query_pairs_mut()
                .append_pair("all", "true")
                .append_pair("since", &(since - Duration::minutes(5)).to_rfc3339())
                .append_pair("per_page", "50")
                .append_pair("page", &page.to_string());
            let response = self.get(url.as_str(), page == 1).await?;
            if response.status() == StatusCode::NOT_MODIFIED {
                break;
            }
            if page == 1 {
                validator = response
                    .headers()
                    .get(header::LAST_MODIFIED)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| (url.to_string(), v.to_owned()));
            }
            let more = has_next(&response);
            notifications.extend(response.json::<Vec<Notification>>().await?);
            if !more {
                break;
            }
        }
        Ok(Batch {
            notifications,
            validator,
        })
    }

    // Only accept the conditional response validator after all subjects have
    // been processed and the database transaction has committed.
    pub fn accept(&mut self, validator: Option<(String, String)>) {
        if validator.is_some() {
            self.validator = validator;
        }
    }

    pub async fn comments(&mut self, subject: &Subject) -> Result<Conversation> {
        let url = subject
            .url
            .as_deref()
            .context("Notification has no subject URL")?;
        let is_pr = subject.kind == "PullRequest";
        let issue_url = if is_pr {
            url.replace("/pulls/", "/issues/")
        } else {
            url.to_owned()
        };
        let description = self
            .get(&issue_url, false)
            .await?
            .json::<RawComment>()
            .await?;
        let mut conversation = Conversation {
            pull_request_author: if is_pr {
                description.user.clone()
            } else {
                None
            },
            ..Conversation::default()
        };
        let pull_request_url = description.html_url.clone();
        conversation
            .comments
            .push(description.into_comment("description", false));
        conversation.comments.extend(
            self.list::<RawComment>(&format!("{issue_url}/comments"))
                .await?
                .into_iter()
                .map(|c| c.into_comment("issue", false)),
        );
        if is_pr {
            conversation.review_comments = self
                .list::<RawComment>(&format!("{url}/comments"))
                .await?
                .into_iter()
                .map(|c| c.into_comment("review", true))
                .collect();
            conversation
                .comments
                .extend(conversation.review_comments.iter().cloned());
            conversation.reviews = self
                .list::<Review>(&format!("{url}/reviews"))
                .await?
                .into_iter()
                .filter_map(Review::into_comment)
                .collect();
            conversation.review_requests = self
                .list::<TimelineEvent>(&format!("{issue_url}/timeline"))
                .await?
                .into_iter()
                .filter_map(|event| event.into_request(&pull_request_url))
                .collect();
        }
        Ok(conversation)
    }
}

fn has_next(response: &Response) -> bool {
    response
        .headers()
        .get(header::LINK)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|links| links.split(',').any(|link| link.contains("rel=\"next\"")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_submitted_reviews_preserve_state_and_identity() -> Result<()> {
        for state in ["APPROVED", "CHANGES_REQUESTED", "COMMENTED"] {
            let review: Review = serde_json::from_value(json!({
                "id": 42, "user": {"login": "reviewer"}, "body": null,
                "html_url": "https://github.com/a/b/pull/1#pullrequestreview-42",
                "submitted_at": "2026-10-30T12:00:00Z", "state": state
            }))?;
            let (comment, parsed_state) = review.into_comment().unwrap();
            assert_eq!(comment.id, "review-body:42");
            assert!(comment.body.is_empty());
            assert!(matches!(
                parsed_state,
                ReviewState::Approved | ReviewState::ChangesRequested | ReviewState::Commented
            ));
        }
        let pending: Review = serde_json::from_value(json!({
            "id": 43, "user": {"login": "reviewer"}, "body": "Draft",
            "html_url": "https://github.com/a/b/pull/1", "submitted_at": null, "state": "PENDING"
        }))?;
        assert!(pending.into_comment().is_none());
        Ok(())
    }

    #[test]
    fn timeline_requests_have_event_identity_and_ignore_unrelated_shapes() -> Result<()> {
        let events: Vec<TimelineEvent> = serde_json::from_value(json!([
            {"event": "committed", "sha": "abc"},
            {"id": 101, "event": "review_requested", "actor": {"login": "author"},
             "requested_reviewer": {"login": "warren"}, "created_at": "2026-10-30T12:00:00Z"},
            {"id": 102, "event": "review_request_removed"},
            {"id": 103, "event": "review_requested", "actor": {"login": "author"},
             "requested_reviewer": {"login": "warren"}, "created_at": "2026-10-30T12:01:00Z"},
            {"id": 104, "event": "review_requested", "actor": null,
             "requested_reviewer": null, "requested_team": {"slug": "team"},
             "created_at": "2026-10-30T12:02:00Z"}
        ]))?;
        let requests: Vec<_> = events
            .into_iter()
            .filter_map(|e| e.into_request("https://github.com/a/b/pull/1"))
            .collect();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].comment.id, "review-request:101");
        assert_eq!(requests[1].comment.id, "review-request:103");
        assert_eq!(requests[0].comment.user.login, "author");
        assert_eq!(requests[0].reviewer.login, "warren");
        assert_eq!(
            requests[0].comment.html_url,
            "https://github.com/a/b/pull/1"
        );
        Ok(())
    }
}
