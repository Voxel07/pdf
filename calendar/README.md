# DIY Calendar

A standalone Bun, Vite, and Svelte calendar with authentik sign-in, a shared calendar plus personal calendars for each user, an event editor, a JSON API, iCalendar feeds, and outgoing webhooks. It uses the same look and light/dark themes as pdfsign.

## Run with Docker

From this directory, copy `.env.example` to `.env`. Generate two distinct secrets with the following command, then set `CALENDAR_ADMIN_TOKEN` and `CALENDAR_FEED_TOKEN` in `.env`.

```sh
bun -e "for (let i = 0; i < 2; i++) console.log(crypto.randomUUID().replaceAll('-', ''))"
docker compose up -d --build
```

Set up authentik sign-in as described below, then open the calendar and choose **Sign in with authentik**. The admin token no longer signs in to the web app. It remains the credential for the [event API](#event-api). The Compose deployment stores events and pending webhooks in the `calendar-data` volume. For remote access and external calendar subscriptions, put an HTTPS reverse proxy in front of the container and set `CALENDAR_PUBLIC_URL` to the resulting origin, such as `https://calendar.example.com`. Route the app from the root of that origin. The subscription URL must be reachable over the internet without a sign-in prompt.

## Sign in with authentik

1. In authentik, create an **OAuth2/OpenID Provider** of client type *Confidential* with the redirect URI `<CALENDAR_PUBLIC_URL>/auth/callback`, for example `https://calendar.example.com/auth/callback`. Keep the default `openid`, `profile`, and `email` scope mappings. The profile mapping supplies the `groups` claim.
2. Create an **Application** for the provider. Bind policies there if only some users should see it.
3. Set `CALENDAR_PUBLIC_URL`, `CALENDAR_OIDC_ISSUER` (the provider's *OpenID Configuration Issuer*, such as `https://auth.example.com/application/o/calendar/`), `CALENDAR_OIDC_CLIENT_ID`, and `CALENDAR_OIDC_CLIENT_SECRET`, then restart the calendar.

| Variable | Default | Effect |
| --- | --- | --- |
| `CALENDAR_OIDC_GROUP` | — | Only members of this group can sign in |
| `CALENDAR_OIDC_ADMIN_GROUP` | — | Only members of this group can change the shared calendar in the web app. Leave it empty to let everyone who signs in edit it |
| `CALENDAR_OIDC_SCOPES` | `openid profile email` | Scopes requested at sign-in |

Group membership is read at each sign-in. A session lasts 12 hours. **Sign out** also ends the authentik session. For local development, set `CALENDAR_PUBLIC_URL=http://localhost:5174` and register `http://localhost:5174/auth/callback` in authentik.

## Calendars

Everyone who signs in sees the shared calendar, named by `CALENDAR_NAME`. pdfsign, the admin token API, and the `CALENDAR_FEED_TOKEN` feed use only this calendar. Each user also gets a **Personal** calendar on their first sign-in and can add, rename, and delete more under **Calendars**, up to 20. Personal calendars are visible only to their owner, never reach pdfsign or webhooks, and are not readable with the admin token. Pick the calendar to show from the switcher in the header. The browser remembers the last one.

In the month view, click a day to create an event on it, or press and drag across several days to create a multi-day all-day event. On a touch screen, tap a day to show its agenda and tap it again to create an event there. Calendars you can only view show events read-only.

## Install DIY Calendar as a web app

Open the calendar's HTTPS URL. On iPhone, choose **Share → Add to Home Screen** in Safari or Chrome. In Safari, enable **Open as Web App** if shown. On Android, choose **Install** from Chrome's menu. On desktop Chrome, select the install icon in the address bar or choose **Install page as app** from the Chrome menu. Open **Calendar** from your home screen or app launcher and sign in with authentik when prompted.

## Connect pdfsign and Discord

The standalone calendar can start with just its admin and feed tokens. Configure a distinct `CALENDAR_PDFSIGN_TOKEN` in both apps to let pdfsign manage the same events, RSVPs, and scoped credentials in `calendar.sqlite`. See the [combined deployment instructions](../README.md#3-event-scheduling). Both Compose entry points default to the volume named `calendar_calendar-data`. If your existing standalone deployment used a different project or volume name, set `CALENDAR_VOLUME_NAME` to that existing volume name. Run a single calendar service per volume so one worker delivers each webhook.

For a connected deployment, [pdfsign backups](../README.md#backups) include a consistent snapshot of this database. Restoring a snapshot retires scoped calendar credentials and clears queued webhook deliveries. Standalone environment tokens remain configured outside the database.

## Subscribe

Use **Subscribe** in the app to copy the read-only iCalendar URL of the open calendar. Each calendar has its own link. In Google Calendar on a computer, add it with **Other calendars → From URL**. In Apple Calendar on iPhone, use **Calendars → Add Calendar → Add Subscription Calendar**. Event changes appear after the recipient app refreshes its feed. The link includes a token that grants access to all published event details. Personal calendar links are derived from `CALENDAR_FEED_TOKEN`. Rotating it invalidates every link, shared and personal, and requires subscribers to add the new ones. Deleting a calendar disables its link.

## Event API

Send `Authorization: Bearer <CALENDAR_ADMIN_TOKEN>` with every request. The admin token reads and writes only the shared calendar. Use `Content-Type: application/json` for requests with a body.

| Method | Path | Response |
| --- | --- | --- |
| GET | `/api/v1/events` | `{ "events": [...] }`, sorted by start |
| POST | `/api/v1/events` | Created event, HTTP 201 |
| GET | `/api/v1/events/:id` | Event |
| PUT | `/api/v1/events/:id` | Replaced event |
| PATCH | `/api/v1/events/:id` | Updated event |
| DELETE | `/api/v1/events/:id` | HTTP 204 |

Create and replace require `title`, `starts_at`, and `ends_at`. Optional fields include `description`, `location`, `all_day` (defaults to `false`), and `status` (`scheduled` or `cancelled`). A new event is scheduled by default. Replacing an existing event without `status` keeps its current status. Timed events accept ISO timestamps with seconds and a UTC offset. Responses normalize timed events to UTC. All-day events use `YYYY-MM-DD` dates with an exclusive end. To create a one-day event, set `ends_at` to the next date.

```json
{
  "title": "Team planning",
  "starts_at": "2027-02-02T10:00:00+01:00",
  "ends_at": "2027-02-02T11:00:00+01:00",
  "location": "Meeting room"
}
```

Use the response's `ETag` as `If-Match` on edits and deletes to detect a stale revision (HTTP 412). For retried creates, send the same `Idempotency-Key` and body. A replay returns the original event with HTTP 200. Reusing a key with different data returns HTTP 409.

## Outgoing webhooks

Set `CALENDAR_WEBHOOK_URL` to the API endpoint that should receive changes. Changes to the shared calendar, from the calendar or pdfsign, send a JSON POST after the change is committed. The payload contains `id` (the delivery ID), `type` (`event.created`, `event.updated`, or `event.deleted`), `occurred_at`, and `event` (the event snapshot, including its revision). A pdfsign cancellation produces `event.updated` with `status: "cancelled"`. Saving identical values through the standalone calendar leaves the revision unchanged and produces no webhook.

The receiver should return a 2xx response and use `X-Calendar-Delivery` to deduplicate retries. The same ID is present in the body. The queue survives restarts and tries up to eight times. Later deliveries may arrive while an earlier one is waiting for retry, so consumers maintaining a mirror should use the event revision to disregard older changes.

`CALENDAR_WEBHOOK_BEARER_TOKEN` adds an Authorization bearer header. `CALENDAR_WEBHOOK_SECRET` adds `X-Calendar-Timestamp` (Unix seconds) and `X-Calendar-Signature` (`sha256=<hex>`). Verify the signature using HMAC-SHA256 over the timestamp, a dot, and the raw request body. Use the same secret on both ends.

## Develop locally

Create `.env` as above, including the authentik settings for `http://localhost:5174`, then run `bun install --frozen-lockfile`. Start `bun run dev:api` and `bun run dev` in separate terminals. Vite proxies requests to the Bun server and prints the frontend URL. Run `bun run check`, `bun test`, and `bun run build` from this directory.

For the cross-service Rust test, start a calendar API with a disposable `CALENDAR_DATA_DIR` and the pdfsign integration enabled. In the Rust test environment set `PDFSIGN_TEST_CALENDAR_URL`, `PDFSIGN_TEST_CALENDAR_TOKEN` (the service token), and `PDFSIGN_TEST_CALENDAR_ADMIN_TOKEN`. From `server/`, run `cargo test --locked event_access_and_calendar_share_the_calendar_database -- --ignored`.
