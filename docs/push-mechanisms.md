# GitHub push mechanisms

Investigated on 2026-10-28 against GitHub's official documentation.

## Findings

GitHub explicitly documents that personal notifications and mentions have no
webhook. Webhooks are scoped to repositories, organizations, or installed GitHub
Apps, with the corresponding administrative or installation access requirements.
Signing into an application does not grant a webhook feed for every repository
where the user happens to participate.

Source: [Types of webhooks](https://docs.github.com/en/webhooks/types-of-webhooks).

Repository events do cover the content needed for filtering:

| Event | Relevant content |
| --- | --- |
| `issue_comment` | Top-level issue and PR comments |
| `pull_request_review_comment` | Inline review comments and replies |
| `pull_request_review` | Submitted review summaries |
| `issues`, `pull_request` | Descriptions and their edits |
| `discussion_comment` | Discussion comments, for future coverage |

`pull_request_review_thread` is about resolution state; it does not replace the
comment event. Event availability and GitHub App permissions are documented per
event.

Source: [Webhook events and payloads](https://docs.github.com/en/webhooks/webhook-events-and-payloads).

The notifications REST API explicitly supports conditional polling, reports a
minimum polling interval, and can include already-read notifications. It is not
an append-only comment event log, so we fetch the subject's actual comments.

Source: [Notifications API](https://docs.github.com/en/rest/activity/notifications).

## Decision for this first implementation

Use authenticated polling for the user's cross-repository notification feed.
It needs no repository setup, inbound listener, or hosted relay. Respect
`X-Poll-Interval`, use conditional requests, cache processed subject versions,
and back off on errors. There is no documented user-level push substitute that
preserves the same coverage.

An optional webhook path can later wake collection immediately for selected
repositories. Both paths should feed the same filtering and comment-ID
deduplication logic, with polling retained for repositories outside webhook
coverage and for reconciliation.

That path would require a publicly reachable HTTPS receiver (or a relay to the
desktop), signature verification, delivery deduplication, and durable storage
while the desktop is offline. None is provisioned by this scaffold.

GitHub does not automatically redeliver failed webhook deliveries. A webhook-only
design therefore needs explicit recovery rather than assuming push is a reliable
event queue.

Sources: [Webhook best practices](https://docs.github.com/en/webhooks/using-webhooks/best-practices-for-using-webhooks),
[Handling failed deliveries](https://docs.github.com/en/webhooks/using-webhooks/handling-failed-webhook-deliveries).
