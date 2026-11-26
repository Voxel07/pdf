# Changelog

All notable changes to pdfsign Tracker are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/Voxel07/pdf/compare/0.0.3...HEAD
[0.0.3]: https://github.com/Voxel07/pdf/compare/0.0.2...0.0.3
[0.0.2]: https://github.com/Voxel07/pdf/compare/0.0.1...0.0.2
[0.0.1]: https://github.com/Voxel07/pdf/releases/tag/0.0.1
