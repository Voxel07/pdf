# pdfsign

A self-hosted alternative to DocuSeal, built for Discord communities.

* **Templates:** upload a PDF and drag fields onto it: text, date, number, select (fixed list) and hand-drawn signatures. A new template can start as a copy of an existing one (PDF, fields, sections, settings), e.g. for next year's form.
* **Sections:** a form is split into sections. Each section is signed and locked on its own. Required sections must be done on the first signing. Optional sections can be filled later from the same link, for example one row per equipment hand-out.
* **Name check:** signing links are random tokens. Opening one also requires the signer's registered full name. Matching ignores case, accents and ü/ue-style spellings. After 10 wrong names (configurable) the link is locked. The name is not secret, so this is a check on top of the link, not an independent authentication factor: whoever holds the link and knows the name can sign.
* **Validity:** a template can be valid for N days (for example 365 for an event waiver). The bot asks for a new signature only when no valid document exists.
* **Lightweight storage:** SQLite. Values are stored as text and signatures as small PNGs. PDFs are rebuilt on demand, with an audit page listing every signing round (time, IP, user agent, name check).
* **Export:** on request, the signed documents of all templates or of one template are rebuilt and written as PDFs to disk (`export/<template>/<date>_<name>_<id>.pdf` plus `index.csv`), from the UI or the CLI, or downloaded as a ZIP. Nothing is exported automatically. Any document can be previewed in the browser before it is downloaded.
* **DocuSeal import:** templates (PDF + fields) and completed submissions (values + signature images) are pulled through the DocuSeal API, and the signed PDFs are rebuilt here. The import runs in the background with a progress bar. You can also keep DocuSeal's original signed PDFs.
* **Activity log:** recent actions (templates, documents, people, signing, bot, import, export, backups) and every server error, filterable in the admin UI under **Activity**.
* **Light and dark theme:** follows the operating system; the admin UI has a toggle.
* **General links + QR codes:** one link per template for everyone, e.g. printed at the entrance. People scan it, type their name and zip code, and either sign on the spot or see that they already have a valid signature. The zip code must match the one on file, so a name alone is not enough to sign for someone.
* **Backups:** scheduled snapshots of signing data and template PDFs, plus the calendar database when events are enabled. Restores invalidate credentials and signing links from the snapshot.
* **Optional Discord bot:** handles waiver checks, equipment hand-outs, relinking a lost account, and opening authenticated event commands.
* **Optional events:** schedule events, share RSVP links, publish a subscribable calendar, and manage events via a scoped API. Event records, RSVPs, calendar credentials, and event API keys use separate SQLite storage.
* **Auth:** login goes through **authentik** (OIDC). Roles come from authentik groups. Public signing pages (`/s/<token>` and `/p/<token>`) use the signing-link flow.

```
server/   Rust backend (axum, sqlx/SQLite, lopdf)
web/      Frontend (Bun, Vite, Svelte 5, pdf.js)
bot/      Discord bot (Bun, discord.js)
```

## Deployment (Docker)

```bash
cp .env.example .env        # fill in the values, see below
mkdir -p data export
docker compose up -d --build pdfsign
```

Put a TLS reverse proxy in front of port 8080 and set `PUBLIC_URL` to the public HTTPS origin. Set `TRUSTED_PROXIES` to the exact proxy peer addresses as seen by pdfsign, and configure those proxies to set or append the client address to `X-Forwarded-For`. With the default empty list, every request uses the connecting peer's address for rate limiting and audit records. This can group all signers behind an unconfigured proxy under one limit.

To run signing on its own, start the `pdfsign` service. Docker Compose starts the bot only when explicitly requested or when the `bot` profile is active.

### Install pdfsign as a web app

Open `/admin` on the HTTPS deployment and sign in. Install from your browser, then open **pdfsign** from your home screen or app launcher.

- **iPhone** — In Safari, use **Share → Add to Home Screen**, enable **Open as Web App** if shown, and tap **Add**. Chrome also offers **Share → Add to Home Screen**.
- **Android** — In Chrome, open the menu and choose **Install**. Depending on the Chrome version, it may be under **Add to Home screen** or **Install and create shortcut**.
- **Desktop Chrome** — Select the install icon in the address bar or use **Menu → Cast, save and share → Install page as app**.

The app opens the admin entry point. To sign a document, open the signing link you received.

### Prebuilt images

GitHub Actions runs checks on pull requests and release tags such as `0.5.3`. To publish a release, push a numeric `major.minor.patch` tag on a commit containing [the workflow](.github/workflows/ci.yml). After the checks pass, GitHub Actions pushes `ghcr.io/voxel07/pdf` and `ghcr.io/voxel07/pdf-bot` with the full version, `major.minor`, and `latest` tags.

Set `PDFSIGN_IMAGE`, `PDFSIGN_BOT_IMAGE`, and `PDFSIGN_TAG` in `.env` using the examples in [.env.example](.env.example). For private packages, authenticate Docker to `ghcr.io` with a token that has package read access. [GitHub Container Registry authentication](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry#authenticating-to-the-container-registry)

```bash
docker compose pull pdfsign bot
docker compose up -d --no-build pdfsign bot
```

### 1. authentik

1. **Applications → Providers → Create → OAuth2/OpenID Provider**
   * Client type: *Confidential*
   * Redirect URI: `https://sign.example.com/auth/callback` (your `PUBLIC_URL` + `/auth/callback`)
   * Scopes: keep the defaults `openid`, `email`, `profile`. authentik's default profile mapping already includes the `groups` claim.
2. **Applications → Create** an application using this provider, with slug `pdfsign`.
3. Create two groups and add your users to them:
   * `pdfsign-admins` – everything, including API tokens, DocuSeal import, export to disk and deleting data
   * `pdfsign-staff` – templates, documents, people, sending links, ZIP export
   These two groups grant signing access. To use other names, set `OIDC_ADMIN_GROUP` / `OIDC_STAFF_GROUP`. An optional third group can grant event viewing and RSVP access.
4. Put the values into `.env`:
   ```
   OIDC_ISSUER=https://auth.example.com/application/o/pdfsign/
   OIDC_CLIENT_ID=...
   OIDC_CLIENT_SECRET=...
   ```

Use the exact HTTPS `issuer` in authentik's discovery document, including the trailing slash if present. Discovery endpoints must share that origin. pdfsign verifies the ID token signature with authentik's published signing key or the client secret for HS256. Sessions last 15 minutes and roles are checked again on login. For immediate offboarding, remove the user's authentik group access and select **Revoke sessions** under **API & export → Users**.

### 2. Discord bot

1. Create an application and bot at <https://discord.com/developers/applications>. Under **Bot**, enable **Message Content Intent**. Install it to your server using the `bot` scope. Give it **View Channel** and **Send Messages** where you want it to answer commands or post staff notifications.
2. To enable signing commands, log into pdfsign as an admin. Go to **API & export → Generate token** and put the token in `PDFSIGN_API_KEY`. For an events-only bot, enable `EVENTS_ENABLED` and leave the signing API key blank.
3. Set `DISCORD_TOKEN` to the Discord bot token and set the template slugs `EVENT_TEMPLATE` / `EQUIPMENT_TEMPLATE`. Set `DISCORD_GUILD_ID` to limit server commands to one server, or leave it blank to serve every server the bot joins.
4. Run `docker compose up -d bot`, then type `!help` in Discord. Unknown `!` commands also show the help menu.

The bot sends personal details and signing links by DM. Enable DMs from server members or use member commands in a direct message with the bot. Staff commands run in the server and send their results by DM. Staff can use an @mention or Discord user ID for a member argument.

| Command | Who | What |
| --- | --- | --- |
| `!help` | everyone | Show the command menu |
| `!register` | everyone | Open a private form to register your full name and ZIP code. A registered name can only be changed by staff, and a ZIP code is only added when none is on file |
| `!signup [event]` | everyone | Checks for a valid waiver. If there is none, you get a signing link |
| *"Interested" on a Discord scheduled event* | everyone | Same as `!signup`, delivered by DM. Staff are notified in `NOTIFY_CHANNEL_ID` if DMs are closed |
| `!equipment` | everyone | Opens your equipment form again to add items |
| `!documents` | everyone | Your documents, their status and links |
| `!handout @member [items]` | staff | DMs the recipient their equipment form (asks them to register first if needed) |
| `!docs status @member` / `!docs send @member <template-slug>` | staff | Shows a member's documents / sends a link for any template |
| `!link-account <old-user-id> @new-member` | staff | Moves all data of a lost Discord account to the new one |

Staff commands require the *Manage Server* permission. If `ADMIN_ROLE_IDS` is set, they require one of those roles instead, and *Manage Server* alone no longer counts.

### 3. Event scheduling

Set `EVENTS_ENABLED=true` in the root `.env`. Generate three distinct URL-safe secrets of at least 32 characters for `CALENDAR_ADMIN_TOKEN`, `CALENDAR_FEED_TOKEN`, and `CALENDAR_PDFSIGN_TOKEN`. Run `docker compose --profile calendar up -d --build calendar pdfsign bot` to start the combined deployment. Drop `bot` from the command if you only want the web apps.

To use an existing standalone calendar, set `CALENDAR_URL` in pdfsign to the calendar origin reachable from the pdfsign container. Set the same `CALENDAR_PDFSIGN_TOKEN` on both services, restart the calendar, and start pdfsign and the bot with `docker compose up -d --build pdfsign bot`. Use HTTPS if the connection leaves a trusted private network.

Open **Events** after logging in to pdfsign. Admins and staff can create, edit, and cancel events. To let community members view events and RSVP, create a group in Authentik and set `OIDC_EVENT_GROUP` to its name. This group grants access to events only.

Set the bot's `PUBLIC_URL` to the public HTTPS address of pdfsign. These commands send an Authentik-protected link by DM. The browser uses the same session as the signing app and asks for confirmation before saving or cancelling an event.

| Command | Action |
| --- | --- |
| `!event list` | Browse upcoming, in-progress, ended, and cancelled events |
| `!event create` | Open the new event form |
| `!event show <id>` | Open an event and RSVP |
| `!event edit <id>` | Open the event editor |
| `!event cancel <id>` | Review and confirm cancellation |

The event ID is the last segment of its page URL. Use **Copy event link** to share a link with attendees. Timed events display in each visitor's browser time zone. The event-enabled bot checks the calendar connection before logging in and before sending event links. Signing-only deployments can leave events disabled. The existing Discord scheduled-event watcher continues to handle signing waivers for native Discord events.

#### Calendar subscriptions

An admin can generate and revoke a private iCalendar feed under **Events → Calendars & API**. Use **Subscribe by URL** or **Add by URL** in a calendar client. The feed includes scheduled events and their cancellations, with a 90-day history. Changes from either web app or API appear when the client next refreshes. Keep the URL private because it grants read access to all event titles, times, locations, and descriptions.

#### Event integration API

An admin can issue **Read-only** or **Read/write** event tokens under **Events → Calendars & API**. Send `Authorization: Bearer <event-token>` with each request.

| Request | Effect |
| --- | --- |
| `GET /api/events/v1/events` | Paginated list. Optional `phase`, `limit` (1–100), and `offset`. Use `next_offset` from the response |
| `GET /api/events/v1/events/{id}` | Event details, current phase, and aggregate RSVP counts |
| `POST /api/events/v1/events` | Create an event using a write token |
| `PUT /api/events/v1/events/{id}` | Replace event fields using a write token and the current `revision` |

Create and update bodies use `title`, `starts_at`, `ends_at`, optional `description` and `location`, and `status` (`scheduled` by default or `cancelled`). Timed events use RFC 3339 with an offset, such as `2026-12-07T18:00:00+01:00`. For an all-day event, set `all_day: true` and use `YYYY-MM-DD` dates with an exclusive end. The returned `phase` is `upcoming`, `ongoing`, `ended`, or `cancelled`. On a `409 stale_revision`, fetch the latest record before retrying. Send the same `Idempotency-Key` when retrying a create. Reusing the key with different input returns `409`.

The calendar service stores events, RSVPs, scoped credentials, and pending webhooks in `calendar.sqlite` in the calendar volume. Both apps read and write it through the calendar service. When events are enabled, pdfsign backups include this database.

### Setting up the two use cases

**Event waiver:** create a template with slug `event-waiver` and set *Valid for* to 365 days. Use one required section with the fields Name (autofill: registered name), Date and Signature.

**Equipment:** create a template with slug `equipment` and leave validity empty (forever). Use a required section *Recipient* (name, date, signature). Then build one optional section for the first row: item (*Select* with your fixed equipment list), quantity (number), date and signature. Use **Sections → Repeat below…** to copy that row down the page. On the first hand-out the recipient signs the recipient section plus row 1. On later hand-outs they open the same link (via `/equipment` or the DM from `/handout`), fill in the next free row and sign it. Signed rows cannot be changed.

### General links / QR codes

In the template editor, **General links & QR codes → Create link**, then **QR** → *Print poster*, *PNG* or *SVG*. Links can be disabled or given an expiry date; signed documents stay valid either way. Opening a link:

1. The signer enters first and last name and their zip code.
2. A registered person with that name and zip code: they continue as that person.
3. Nobody with that name and zip code: a new person is created. Each client can create at most 10 new people per 10 minutes.
4. A person with that name but no zip code on file: the signer is asked to get staff help rather than the app guessing. Add the zip code under **People** (the People page marks namesakes without one with *same name · no zip!*).
5. Valid signature already present: "You're all set". Otherwise they sign on the spot. On a shared tablet, *Done — next person* resets the page.

The signing page opened from a QR link uses a temporary `qr-…` address that ends with the two-hour signing session. It never shows the person's personal signing link.

QR identification is limited to 100 requests per client per 10 minutes (IPv6 clients count per /64). Personal-link unlocks are limited to 50 per client per 10 minutes and the configured number of failed attempts per link. An existing locked submission also stays locked when opened through a QR link. Anyone with the shared link who knows a member's name and zip code can sign that person's pending document. Use personal Discord links for sensitive documents.

Behind a reverse proxy, set `TRUSTED_PROXIES` to the proxy's address. Otherwise every visitor shares the proxy's address and its rate limit, and pdfsign logs a warning when proxied requests arrive.

### Backups

Admins configure backups on the **Backups** page: folder, first run time, interval (hours or days) and how many to keep. With Docker, set `BACKUP_HOST_DIR` in `.env` (mounted at `/backups`) and `TZ` for the schedule's timezone.

Use a private backup folder writable by `PDFSIGN_UID`/`PDFSIGN_GID`. Archives are created with mode `0600` on Unix. Use an owner-only ACL for a native Windows backup folder. Archives contain personal data. Exports and DocuSeal originals in `./export` require their own backups.

Each run briefly holds signing writes while SQLite snapshots the database. The template PDFs that snapshot references are copied afterwards; a backup fails (retry it) if one of them is deleted in the meantime. User sessions, signer sessions, and pending OIDC logins are removed from the snapshot. If events are enabled, the calendar service must return a valid SQLite snapshot of at most 128 MiB. Each database snapshot is consistent independently. The app verifies database integrity and checks for missing template PDFs before publishing the ZIP. A manual backup leaves the schedule in place. Day-based intervals stay on their days and start time across DST changes.

Run `pdfsign backup` from the CLI. It uses a separate process, so a running service can continue writing. If a concurrent template change leaves the snapshot incomplete, the backup fails validation and can be retried.

For a signing-only archive, stop pdfsign and restore the ZIP:

```bash
docker compose stop pdfsign
docker compose run --rm pdfsign pdfsign restore /backups/backup-file.zip --force
docker compose start pdfsign
```

For an archive that includes the calendar, stop both services and temporarily mount the calendar volume for the restore. Replace `calendar_calendar-data` if you configured `CALENDAR_VOLUME_NAME`. The restore user and calendar service must have access to the restored database. The default Compose UIDs match.

```bash
docker compose --profile calendar stop pdfsign calendar
docker compose run --rm -v calendar_calendar-data:/calendar-data pdfsign \
  pdfsign restore /backups/backup-file.zip --force --calendar-data-dir /calendar-data
docker compose --profile calendar start calendar pdfsign
```

Only archives made by this pdfsign version can be restored. Pass `--signing-only` to restore just signing data from a combined archive. The restore validates all selected data before replacing live files and retains the previous data in private `.pre-restore-*` folders. Reissue signing API keys, personal links, and scoped calendar tokens or feeds after a restore. General links are disabled and must be re-enabled and shared using their new URLs. Pending calendar webhooks are discarded to avoid replaying changes.

### Import from DocuSeal

Go to **DocuSeal import** (admin only) and enter the API URL and API key:

* `https://api.docuseal.com` or `https://api.docuseal.eu` for the cloud
* `https://<host>/api` for self-hosted DocuSeal

Pick templates and import them. How things map:

* Multi-document templates are merged into one PDF.
* Each DocuSeal submitter role becomes a section.
* Field types `text`, `number`, `date`, `select`/`radio`, `checkbox` (drawn as "X"), and `signature`/`initials`/`image`/`stamp` are imported. Other types are reported and skipped.
* Submitters are matched to existing people by Discord ID (`external_id` or `metadata.discord_id`), then e-mail, then a name that exactly one person has (unless that person's e-mail or Discord account says it is someone else). Everyone else becomes a new person; duplicates can be merged under **People**.
* Only submissions that every submitter has completed are imported. Partly completed ones are skipped and picked up by a later import.
* The import runs on the server in the background, so closing the page or a proxy timeout does not stop it. The page shows its progress and the result when you come back. DocuSeal rate limits (429) and temporary errors are retried.
* Each submission (person, values, signing rounds) is written as one unit, so an interrupted import can simply be repeated. Importing again skips anything already imported. Imported IDs are qualified by the DocuSeal host, so two instances don't collide.
* The result lists how many submissions were imported, already imported, not completed yet, or failed. Every warning and error is also written to the activity log (**Activity**, area `import`).
* The API key stays on the exact configured origin. Each file destination and redirect must resolve to public addresses over HTTPS unless its exact origin is set in `DOCUSEAL_TRUSTED_ORIGINS`. Configure only private instances or storage endpoints you operate. Individual PDFs are limited to 32 MiB and API responses and signature images to 8 MiB.

Then export the PDFs from the panel below the result.

### Export / backup

* UI: Dashboard or **API & export** → pick *All templates* or one template → *Export to server disk* (admin) or *Download ZIP*. A progress bar shows the documents being rendered. The ZIP is built on the server and then downloaded. Documents that fail to render are reported and logged. In the ZIP they are listed in `FAILED.txt`.
* CLI: `docker compose exec pdfsign pdfsign export [--out /export/somewhere] [--template <slug>]`. It exits non-zero if any document failed.
* A disk export writes the current documents and a new `index.csv`. It does not delete files from earlier exports, such as revoked documents or files under an old name.
* Back up `./data` (SQLite database + template PDFs). `./export` holds the generated PDFs.

## Bot / integration API

All endpoints need `Authorization: Bearer <token>`.

```
GET  /api/v1/templates
GET  /api/v1/persons/{discord_id}                          person, linked accounts, documents
PUT  /api/v1/persons/{discord_id}        {name, username, zip}  register
        A registered person keeps their name (409 already_registered for a different one)
        and their ZIP code (only added when none is on file).
GET  /api/v1/persons/{discord_id}/status?template={slug}   {registered, valid, valid_until, submission}
POST /api/v1/links   {template, discord_id, name?, username?, force_new?}
     -> {url, status, valid, valid_until, created, ...}
        Reuses the open or still-valid document, otherwise creates a new one.
        Returns 409 name_required if the Discord user is not registered and no name is given.
POST /api/v1/persons/link   {old_discord_id, new_discord_id, new_username?}
GET  /api/v1/submissions/{id}
GET  /api/v1/submissions/{id}/pdf
```

## Configuration

| Variable | Default | |
| --- | --- | --- |
| `PUBLIC_URL` | `http://localhost:8080` | Base for signing links and the OIDC redirect |
| `OIDC_ISSUER`, `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET` | – | authentik provider |
| `OIDC_SCOPES` | `openid profile email` | |
| `OIDC_ADMIN_GROUP` / `OIDC_STAFF_GROUP` | `pdfsign-admins` / `pdfsign-staff` | |
| `OIDC_EVENT_GROUP` | unset | authentik group for viewing events and RSVPing |
| `EVENTS_ENABLED` | `false` | Enables event commands, API, and feeds backed by the calendar service |
| `CALENDAR_URL` | `http://calendar:3090` in Compose | Internal HTTP(S) origin for the calendar service |
| `CALENDAR_PDFSIGN_TOKEN` | — | Shared service credential configured in both pdfsign and the calendar |
| `DATA_DIR` / `EXPORT_DIR` | `/data` / `/export` | |
| `MAX_NAME_ATTEMPTS` | `10` | Wrong names before a link is locked. Staff can unlock it or issue a new link |
| `NO_AUDIT_PAGE` | unset | Set to `true` to leave out the audit page |
| `BACKUP_DIR` | `/backups` | Default backup folder inside the container (compose mounts `BACKUP_HOST_DIR` there) |
| `TZ` | `UTC` | Timezone for backup schedules |
| `TRUSTED_PROXIES` | empty | Comma-separated peer addresses or narrow CIDRs trusted to supply `X-Forwarded-For` for rate limits and audit IPs |
| `DOCUSEAL_TRUSTED_ORIGINS` | empty | Comma-separated exact origins allowed for private or plain HTTP DocuSeal and storage endpoints |

## Development

```bash
# backend (or scripts/docker-cargo.sh if no C toolchain is installed)
cd server && PUBLIC_URL=http://localhost:5173 OIDC_ISSUER=... OIDC_CLIENT_ID=... OIDC_CLIENT_SECRET=... cargo run
# frontend with hot reload (proxies /api and /auth to :8080)
cd web && bun install && bun run dev
# bot
cd bot && bun install && bun run start
```

Text is drawn with the PDF standard Helvetica font (WinAnsi). Latin characters including umlauts work. Characters outside that set are shown as `?`.
