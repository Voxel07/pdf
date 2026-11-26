# Changelog

All notable changes to pdfsign Tracker are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5] - 2026-11-26

### Added

- pdfsign and DIY Calendar can be installed as standalone web apps on iPhone, Android, and desktop Chrome.
- Releases publish a calendar image, `ghcr.io/voxel07/pdf-calendar`. Set `PDFSIGN_CALENDAR_IMAGE` to use it with Docker Compose. A locally built calendar image is now tagged `pdfsign-calendar`.
- Personal calendars: each user gets a private **Personal** calendar on first sign-in, can add more, and switches between them and the shared calendar from the header. Each has its own subscription link. pdfsign, the admin token API, and webhooks still see only the shared calendar, which holds all existing events.
- Click a day in the month view to create an event there, or drag across days to create a multi-day event.

### Changed

- **DIY Calendar sign-in uses authentik.** The web app no longer accepts the admin token. Create an authentik OAuth2/OpenID provider with the redirect URI `<CALENDAR_PUBLIC_URL>/auth/callback`, then set `CALENDAR_PUBLIC_URL`, `CALENDAR_OIDC_ISSUER`, `CALENDAR_OIDC_CLIENT_ID`, and `CALENDAR_OIDC_CLIENT_SECRET`. Optional `CALENDAR_OIDC_GROUP` limits who can sign in, and `CALENDAR_OIDC_ADMIN_GROUP` limits who can edit the shared calendar. The event API still uses `CALENDAR_ADMIN_TOKEN`. See [the calendar README](calendar/README.md#sign-in-with-authentik).
- DIY Calendar now uses pdfsign's look, including its light and dark themes and theme switch.
- Calendar notifications, such as a deleted event, appear as a snackbar that disappears after a few seconds.

### Fixed

- pdfsign now stops with a clear message when its data folder holds a database from 0.0.3 or earlier, instead of failing with `no such column: name_key`.
- When event commands are unavailable, the bot's startup error now names the cause: pdfsign unreachable, events disabled in pdfsign, or calendar unavailable. pdfsign logs why it cannot reach the calendar: `CALENDAR_URL` or `CALENDAR_PDFSIGN_TOKEN` missing, the calendar unreachable, or the token rejected.

## [0.0.4] - 2026-11-26

### Upgrading

- **0.0.4 needs a fresh data folder.** pdfsign no longer migrates databases from earlier versions, and `pdfsign restore` refuses backups from 0.0.3 and earlier. Start with an empty `DATA_DIR` and re-import from DocuSeal if needed. The calendar database is new in this release.
- Docker Compose no longer starts the bot by default. Start it with `docker compose up -d bot`. For events, see the README's `--profile calendar` command.
- `TRUSTED_PROXIES` now defaults to empty instead of loopback and private networks, and `X-Real-IP` is no longer read. Behind a reverse proxy, set `TRUSTED_PROXIES` to the proxy's address and make the proxy send `X-Forwarded-For`. Otherwise all visitors share one rate limit, and pdfsign logs a warning.
- With authentik configured, pdfsign refuses to start unless `PUBLIC_URL` is an HTTPS origin without a path. Plain HTTP is accepted only for `localhost`, `127.0.0.1` and `[::1]`. `MAX_NAME_ATTEMPTS` must be greater than 0.
- QR links now need a zip code on file. Before relying on a QR link, add zip codes under **People** for people who were created by staff or imported without one.
- Restoring a backup now disables all general (QR) links and gives them new URLs, so printed QR codes stop working until you re-enable the links and share them again. Restoring also deletes all API keys, so the bot needs a new `PDFSIGN_API_KEY`, and gives every personal signing link a new token.
- A bot with `EVENTS_ENABLED=true` requires `PUBLIC_URL`.
- DocuSeal imports ignore `HTTP_PROXY` and `HTTPS_PROXY`. pdfsign must reach DocuSeal and its file storage directly.
- The container image creates `/data`, `/export` and `/backups` owned by user 10001 with mode `0700`. The bundled Compose file uses bind mounts and is unaffected. With named volumes and another `user:`, fix the volume ownership.

### Added

- The new calendar service runs standalone. It has a month view, a daily agenda, an event editor, an authenticated API, an iCalendar feed, and outgoing webhooks. Each webhook delivery is attempted up to eight times, and pending deliveries survive a restart.
- With `EVENTS_ENABLED=true`, pdfsign manages events and RSVPs under **Events**. It reads and writes the calendar's SQLite database through the calendar service, using a shared `CALENDAR_PDFSIGN_TOKEN`. Admins and staff can create, edit, and cancel events. pdfsign does not delete events. To let other members view events and RSVP, set `OIDC_EVENT_GROUP` to an authentik group. Members of that group can open only the event pages.
- Under **Events → Calendars & API**, admins can generate a private iCalendar feed and issue read-only or read/write tokens for `/api/events/v1/events`.
- The Discord bot's `!event list`, `create`, `show`, `edit`, and `cancel` commands send a DM link to the authentik-protected event pages. The bot needs a reachable calendar connected to pdfsign. With events enabled, it exits at startup if the calendar is unreachable and refuses event commands while it is down.
- The bot can run with only event commands. Leave `PDFSIGN_API_KEY` blank and set `EVENTS_ENABLED=true`.
- `pdfsign restore` accepts `--calendar-data-dir` to restore the calendar database from a combined archive, or `--signing-only` to skip it. A combined archive is refused without one of them.
- `DELETE /api/admin/users/{id}/sessions` ends all sessions of a user (admin only).

### Changed

- Docker Compose places the Discord bot in the optional `bot` profile and the calendar in the optional `calendar` profile. Start the bot explicitly with `docker compose up -d bot`, or enable the profile.
- The reset under **API & export** is now labeled **Delete all signing data**. It does not delete calendar events.
- General (QR) links always ask for the zip code together with the name. People with a registered name but no zip code on file must ask staff to add one before they can use a QR link. A name and zip code that match nobody create a new person, even when someone with that name is registered under another zip code. Merge such duplicates under **People**.
- Registering again through the Discord bot with a different name is refused (`409 already_registered` on `PUT /api/v1/persons/{discord_id}`), and a zip code on file is never replaced. Repeating the registered name in another spelling is accepted. Staff change names under **People**.
- Backup archives are named `backup-YYYYMMDD-HHMMSS-mmm-<random>.zip`.
- Template fields accept font sizes up to 96 pt.
- Export and backup folders must be absolute paths outside the published web folder, the data folder itself and its template folder. A relative `DATA_DIR`, `EXPORT_DIR` or `BACKUP_DIR` is resolved against the working directory at startup, so the default folders keep working.
- ZIP exports and backup downloads are streamed from temporary files instead of memory. At most two background exports run at once.

### Removed

- Startup migrations for databases from earlier versions (added columns, name-key backfill and repair).
- The synchronous `POST /api/admin/export` and `GET /api/admin/export.zip`. Use the export jobs under `/api/admin/export/jobs`.
- The bot's startup cleanup of the slash commands from 0.0.2 and earlier, and the `REGISTER_COMMANDS` variable.
- DocuSeal imports no longer recognize records imported before source keys were qualified by the DocuSeal instance.

### Security

- DocuSeal requests validate every destination and cap download sizes (32 MiB per PDF, 8 MiB per API response or signature image). Private or HTTP DocuSeal instances now require an exact origin in `DOCUSEAL_TRUSTED_ORIGINS`. API redirects must stay on the configured origin, and upstream error bodies are no longer shown.
- Authentik discovery requires the exact HTTPS issuer, and ID tokens are signature-verified. New sessions last 15 minutes, and admins can revoke a user's sessions from **API & export**.
- ID tokens must name pdfsign as their only audience, and an `azp` claim must match the client ID. A login without access to any pdfsign group ends that user's existing sessions, and so does a role change.
- Personal signing attempts are counted atomically and rate-limited by client IP. QR links respect the lock on an existing submission. Each client can create at most 10 new people through QR links and download at most 60 signed PDFs per 10 minutes.
- `TRUSTED_PROXIES` no longer trusts private networks by default, and `X-Real-IP` is ignored. Only `X-Forwarded-For` from listed proxies is believed.
- QR sign-in requires the zip code on file, so a name alone no longer opens someone's document. QR sessions use a temporary `qr-…` page address and never return the personal signing link.
- Signature uploads must be PNG images. Images are decoded with size limits (4000 px per side), off the request threads.
- Admin requests that a browser marks as cross-site are refused, and a cross-site link cannot log you out.
- Responses carry a Content-Security-Policy, `X-Frame-Options`, `X-Content-Type-Options`, `Referrer-Policy: no-referrer`, and HSTS for HTTPS deployments.
- Internal server errors no longer include database, file or upstream details in the response. They are recorded in the activity log.
- Exported `index.csv` cells that a spreadsheet would run as a formula are prefixed with an apostrophe.
- The rate limiter groups IPv6 clients by /64 and no longer refuses all new clients when its table is full. Login starts are rate-limited, and pdfsign warns when `X-Forwarded-For` arrives from a proxy missing from `TRUSTED_PROXIES`.
- The container image creates `/data`, `/export` and `/backups` owner-only instead of world-writable. If you run the image with named volumes as a user other than 10001, set the volume ownership yourself.
- CI audits Rust and Bun dependencies for known vulnerabilities.
- New backups use owner-only file permissions on Unix and include calendar data when events are enabled. Backups no longer contain login or signing sessions. Restoring deletes API keys, gives personal signing links new tokens, and disables general links under new URLs. Combined restores also retire calendar credentials and clear queued webhook deliveries.

### Performance

- PDF rendering, template parsing and image decoding run on the blocking thread pool, at most one job per CPU core, so renders no longer stall other requests. Parsed template PDFs are cached.
- QR identification looks names up through an indexed key instead of scanning every person. Exports no longer look up each document's person twice. The template list and the bot's person view use single queries.
- New indexes for signer sessions, signing rounds, general links, user sessions and the activity log. Bot requests update a key's last use at most every five minutes.
- Backups copy template PDFs after the write pause instead of during it. OIDC signing keys are cached for ten minutes.

## [0.0.3] - 2026-11-26

### Added

- `!help` shows the Discord bot’s command menu. Unknown `!` commands show the same menu, with staff commands included for authorized users.
- Staff can create groups and set required templates under **People → Groups & requirements**, which also lists the members who still need a valid signature. Assign people to one or more groups under **People → Manage**. Groups cannot be renamed or deleted yet.
- Clicking a person’s document count opens their documents, including pending, signed, expired, and revoked records. People with identical names have separate document lists. Person names in the Documents list link to the same view.

#### Data deletion

These actions require the admin role.

- One submission — Open it under Documents and select Delete submission. This removes its values, signatures, signing link, and audit trail.
- One template — Open it under Templates and select Permanently delete template. This removes its source PDF, general links, and all associated submissions. Archive template is still available to keep its records.
- All live application data — Under API & export, type DELETE ALL DATA to confirm the reset. It clears records, settings, API tokens, and the activity log and signs everyone out. Separately saved backups, exports, and downloads are preserved. The reset is refused while a backup, DocuSeal import, or export is running.

### Changed

- People and Documents show 50 entries per page and fetch the remaining pages in the background.
- The notification bell on the right of the admin navigation opens the activity log.
- Signing forms prefill matching first name, surname, full name, and ZIP fields from the identification step. Signers can correct these entries before submitting.
- Discord commands now use the `!` prefix. Enable **Message Content Intent** in the Discord Developer Portal before upgrading, and give the bot **View Channel** and **Send Messages** where it should answer commands. The `applications.commands` invite scope is no longer needed.
- The bot removes its legacy pdfsign slash commands on startup and leaves other application commands alone. Set `REGISTER_COMMANDS=false` to skip this cleanup.
- `DISCORD_CLIENT_ID` is no longer used and can be removed from `.env`. The `bun run register` script was removed.
- `!register` opens a private registration form. The bot sends personal details and signing links by DM, including the results of staff commands. In a DM with the bot, `!help` lists member commands only.
- Merging two people keeps the group memberships of both.

### Fixed

- The **Expired** filter on Documents could miss expired documents, because it was applied after the list was cut to 500 entries. The filter now runs in the database.
- Admin logins and ZIP exports could write to the database while a backup snapshot was being taken. They now wait for the snapshot like other changes.

## [0.0.2] - 2026-11-26

### Added

- Activity log in the admin UI (**Activity**): recent actions on templates, documents, people, API tokens, signing, the bot, DocuSeal imports, exports and backups, plus every server error. You can filter it by level, area and text, it refreshes itself, and it keeps the newest 5000 entries. The dashboard shows the latest entries and the error count of the last 24 hours.
- Progress bar for the DocuSeal import: the current template, submission x of y and the current step.
- Progress bar for exports: a disk export or ZIP download shows document x of y while the PDFs are rendered, then "packing" while the ZIP is built. Exports run as background jobs (`POST /api/admin/export/jobs`, `GET /api/admin/export/jobs/{id}`, `GET /api/admin/export/jobs/{id}/download`). The finished ZIP is downloaded once and kept on the server for at most 30 minutes.
- Dark theme (near-black `#020409` background, amber `#764d0f` highlights, smoky white text). It follows the operating system by default, and the toggle in the admin navigation switches between system, dark and light.
- Export of a single template: pick a template in the export panel (Dashboard, **API & export**, after an import) for a disk export or ZIP download, or run `pdfsign export --template <slug>` on the CLI. The ZIP file name includes the template slug.
- In-browser PDF preview before downloading, for documents (detail page and list), the template preview with sample values, and the signed PDF on the signer page.
- Sorting the people list by name, Discord account or number of documents (click a column header to switch between ascending and descending).
- New template based on an existing one (**Templates → New template based on an existing one**, or **Copy as new template** in the editor). It copies the PDF, fields, sections, description and validity, but no documents or general links.
- `GET /api/admin/docuseal/import` (import progress) and `GET /api/admin/activity`. The synchronous `POST /api/admin/export` and `GET /api/admin/export.zip` remain available and also accept a template.

### Changed

- The DocuSeal import runs in the background on the server. `POST /api/admin/docuseal/import` returns right away, and the page polls for progress. You can close the page, and the result is still shown when you come back.
- DocuSeal requests are retried on rate limits (429), server errors and network failures, and `Retry-After` is respected.
- Imported submitters with no known Discord ID or e-mail are matched to an existing person by name when exactly one person has that name and nothing contradicts it. Before, a duplicate person was created. A submitter's Discord ID is now also linked to a person found by e-mail or name.
- The import result counts submissions that were already imported, are not completed yet, or failed. Before, these were all reported as "skipped".
- The import now also fetches archived DocuSeal submissions.
- An import writes each submission under the backup write gate instead of blocking backups for the whole import.

### Fixed

- A DocuSeal import of large templates could stop partway without any error: the import ran inside one HTTP request, and when a reverse proxy or the browser gave up, the server dropped the request and the rest of the import with it. One symptom was a second template with 180 submissions importing only 8. Running the import again now adds the missing submissions.
- Import warnings and errors were only shown in the browser that started the import, and were lost when the request failed. They are now always written to the activity log.

## [0.0.1] - 2026-11-25

### Added

- First release: PDF templates with text, date, number, select and signature fields, and sections that can be signed later.
- Personal signing links with a name check, and general links / QR codes.
- Validity periods, audit page, export to disk and as ZIP.
- DocuSeal import, scheduled backups and restore.
- authentik (OIDC) login with admin and staff roles, and a bot API with API tokens.
- Discord bot.

[Unreleased]: https://github.com/Voxel07/pdf/compare/0.0.4...HEAD
[0.0.4]: https://github.com/Voxel07/pdf/compare/0.0.3...0.0.4
[0.0.3]: https://github.com/Voxel07/pdf/compare/0.0.2...0.0.3
[0.0.2]: https://github.com/Voxel07/pdf/compare/0.0.1...0.0.2
[0.0.1]: https://github.com/Voxel07/pdf/releases/tag/0.0.1
