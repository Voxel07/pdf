# Working on pdfsign

pdfsign is a self-hosted PDF signing app for Discord communities. Start with [README.md](README.md) for product behavior and deployment and [PLAN.md](PLAN.md) for the working-tree review, priorities, and verification gaps.

## Repository map

| Change | Start here | Related code |
| --- | --- | --- |
| HTTP routing, CLI, configuration | [server/src/main.rs](server/src/main.rs) | [state.rs](server/src/state.rs) initializes SQLite, its pool, PDF paths, the backup gate, and the QR limiter. [schema.sql](server/src/schema.sql) is the complete schema, applied at startup. There are no migrations: a schema change needs a fresh database |
| Admin login, staff/admin roles, bot keys | [server/src/auth.rs](server/src/auth.rs) | `Staff`, `AdminUser`, and `ApiClient` are the Axum extractors. [routes/admin.rs](server/src/routes/admin.rs) owns `/api/admin`. [routes/api.rs](server/src/routes/api.rs) owns `/api/v1` |
| People, templates, validity, signing, export | [server/src/service.rs](server/src/service.rs) | [models.rs](server/src/models.rs), [routes/public.rs](server/src/routes/public.rs), and [util.rs](server/src/util.rs) for timestamps, normalization, tokens, and API errors |
| PDF rendering and merge | [server/src/pdf.rs](server/src/pdf.rs) | [web/src/lib/PdfViewer.svelte](web/src/lib/PdfViewer.svelte) and [pdf.ts](web/src/lib/pdf.ts) handle the browser viewport. The editor and signer share normalized field coordinates |
| Public signing UI | [web/src/App.svelte](web/src/App.svelte) | [Sign.svelte](web/src/routes/Sign.svelte) for `/s/:token`, [PublicLink.svelte](web/src/routes/PublicLink.svelte) for `/p/:token`, and [SignaturePad.svelte](web/src/lib/SignaturePad.svelte) for drawing |
| Admin UI and shared client types | [web/src/routes/admin/AdminApp.svelte](web/src/routes/admin/AdminApp.svelte) | [TemplateEditor.svelte](web/src/routes/admin/TemplateEditor.svelte), [GeneralLinks.svelte](web/src/routes/admin/GeneralLinks.svelte), and [web/src/lib/api.ts](web/src/lib/api.ts) |
| Backup, restore, and DocuSeal import | [server/src/backup.rs](server/src/backup.rs) and [docuseal.rs](server/src/docuseal.rs) | [Backups.svelte](web/src/routes/admin/Backups.svelte) and [Import.svelte](web/src/routes/admin/Import.svelte) |
| Discord commands and events | [bot/src/index.ts](bot/src/index.ts) | [commands.ts](bot/src/commands.ts) defines the help menu, [api.ts](bot/src/api.ts) is the bearer client, and [config.ts](bot/src/config.ts) reads env |
| Build and deployment | [docker-compose.yml](docker-compose.yml), [Dockerfile](Dockerfile), and [bot/Dockerfile](bot/Dockerfile) | [.env.example](.env.example), [GitHub Actions workflow](.github/workflows/ci.yml), [renovate.json](renovate.json), and [scripts/docker-cargo.sh](scripts/docker-cargo.sh) |

## Follow the data

- A person can have multiple Discord accounts. A submission belongs to one person and one template. Template fields and sections are stored as JSON in SQLite. The source PDF lives under `DATA_DIR/templates/<template-id>.pdf` and SQLite lives at `DATA_DIR/pdfsign.db`.
- Personal entry starts with a submission link token and a normalized registered-name check. QR entry starts with a template-level general link and resolves a person by name and, when needed, zip code. Both obtain a two-hour signer session and converge on the same form and submit routes. Admin sessions use a separate HttpOnly cookie. The bot uses a bearer token.
- Required sections must be complete on the first signing round. Optional sections can be signed later. `submit_round`, `done_sections`, and `validity` drive the behavior. A continuation keeps the original `completed_at`. Keep the distinction between submission state (`pending`, `completed`, `revoked`) and computed expiry when editing APIs or UI.
- Field positions are fractions of the displayed page, with a top-left origin and zero-based page indices. Keep `models.rs`, `web/src/lib/api.ts`, the editor, and `pdf.rs` aligned when changing field shape, coordinates, or formatting.
- Values and signature PNGs are stored with their signing round. Downloads and exports currently rebuild PDFs from the live template and stored values. Read the record-integrity items in [PLAN.md](PLAN.md) before changing template editing, validity, or audit behavior.
- Backups include SQLite and template PDFs. Rendered exports and optional DocuSeal originals live under `EXPORT_DIR`. Use temporary `DATA_DIR`, `EXPORT_DIR`, and `BACKUP_DIR` for local checks. Restore only with the server stopped and use a fresh destination for test restores.

## Discord commands

- Use the `!` prefix for bot commands. Route unknown `!` commands to the same menu as `!help`, and list staff commands only for users with staff access.
- Keep registration details in the private form and send personal details and signing links by DM. Preserve the staff permission check when adding staff commands.
- When changing bot commands, update the help menu and [README.md](README.md) together. Record user-facing changes and upgrade requirements under **Unreleased** in [CHANGELOG.md](CHANGELOG.md).

## Local checks

Run these from the repository root. GitHub Actions runs the same checks with the versions pinned in [the workflow](.github/workflows/ci.yml).

```bash
(cd server && cargo test --locked)
(cd calendar && bun install --frozen-lockfile)  # web imports calendar/src/lib
(cd web && bun install --frozen-lockfile && bun run check --fail-on-warnings && bun run build)
(cd bot && bun install --frozen-lockfile && bun run check)
```

- Run the backend from `server/` with `cargo run --locked -- serve` and the frontend from `web/` with `bun run dev`. [Vite](web/vite.config.ts) listens on `5173` and proxies `/api` and `/auth` to port `8080`. For local admin login set `PUBLIC_URL=http://localhost:5173` and configure the matching authentik callback and groups.
- `pdfsign create-api-key <name>` creates a bearer key for local integration checks. Run `pdfsign --help` for CLI flags. [bot/src/config.ts](bot/src/config.ts) lists the bot's required variables. Use a test guild for live Discord checks.
- Rust tests cover name and slug normalization, PDF render/merge, backup scheduling, forwarded-IP trust, ID token claims, and (in [tests.rs](server/src/tests.rs), on a disposable data dir) competing and concurrent signing rounds, revocation and token rotation races, reuse-or-create, template validation, and backup/restore. When changing signing or persistence, check first signing, optional continuation, repeat submissions, expiry, and backup/restore through the entry points affected by the patch.
