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

    pub async fn comments(&mut self, subject: &Subject) -> Result<(Vec<Comment>, Vec<Comment>)> {
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
        let mut comments = vec![description.into_comment("description", false)];
        comments.extend(
            self.list::<RawComment>(&format!("{issue_url}/comments"))
                .await?
                .into_iter()
                .map(|c| c.into_comment("issue", false)),
        );
        let mut review_comments = Vec::new();
        if is_pr {
            review_comments = self
                .list::<RawComment>(&format!("{url}/comments"))
                .await?
                .into_iter()
                .map(|c| c.into_comment("review", true))
                .collect();
            comments.extend(review_comments.iter().cloned());
            comments.extend(
                self.list::<Review>(&format!("{url}/reviews"))
                    .await?
                    .into_iter()
                    .filter_map(|r| {
                        Some(Comment {
                            id: format!("review-body:{}", r.id),
                            user: r.user.unwrap_or_default(),
                            body: r.body.unwrap_or_default(),
                            html_url: r.html_url,
                            created_at: r.submitted_at?,
                            thread_id: None,
                        })
                    }),
            );
        }
        Ok((comments, review_comments))
    }
}

fn has_next(response: &Response) -> bool {
    response
        .headers()
        .get(header::LINK)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|links| links.split(',').any(|link| link.contains("rel=\"next\"")))
}
