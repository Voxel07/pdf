# pdfsign

A self-hosted alternative to DocuSeal, built for Discord communities.

* **Templates:** upload a PDF and drag fields onto it: text, date, number, select (fixed list) and hand-drawn signatures.
* **Sections:** a form is split into sections. Each section is signed and locked on its own. Required sections must be done on the first signing. Optional sections can be filled later from the same link, for example one row per equipment hand-out.
* **Name check:** signing links are random tokens. Opening one also requires the signer's registered full name. Matching ignores case, accents and ü/ue-style spellings. After 10 wrong names (configurable) the link is locked. The name is not secret, so this is a check on top of the link, not an independent authentication factor: whoever holds the link and knows the name can sign.
* **Validity:** a template can be valid for N days (for example 365 for an event waiver). The bot asks for a new signature only when no valid document exists.
* **Lightweight storage:** SQLite. Values are stored as text and signatures as small PNGs. PDFs are rebuilt on demand, with an audit page listing every signing round (time, IP, user agent, name check).
* **Export:** on request, every signed document is rebuilt and written as a PDF to disk (`export/<template>/<date>_<name>_<id>.pdf` plus `index.csv`), from the UI or the CLI, or downloaded as a ZIP. Nothing is exported automatically.
* **DocuSeal import:** templates (PDF + fields) and completed submissions (values + signature images) are pulled through the DocuSeal API, and the signed PDFs are rebuilt here. You can also keep DocuSeal's original signed PDFs.
* **General links + QR codes:** one link per template for everyone, e.g. printed at the entrance. People scan it, type their name, and either sign on the spot or see that they already have a valid signature. A zip code is only needed when several people share the name, and it is collected once from first-time signers.
* **Backups:** scheduled (start time + interval + retention) into a host folder. Writes pause briefly while a consistent snapshot of the database (including active sessions) and the template PDFs is taken. The pause grows with the database size.
* **Discord bot:** handles event sign-up checks, equipment hand-outs, and relinking a lost account to a new one.
* **Auth:** login goes through **authentik** (OIDC). Roles come from authentik groups. Bots use API tokens generated in the app. Only the personal signing pages (`/s/<token>`) and general links (`/p/<token>`) work without login.

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

Put a TLS reverse proxy in front of port 8080 and set `PUBLIC_URL` to the public HTTPS URL. Session cookies are marked `Secure` when `PUBLIC_URL` starts with `https://`. The proxy must set `X-Forwarded-For`. The header is only believed from addresses in `TRUSTED_PROXIES` (default: loopback and private networks, which covers a proxy on the host or in Docker).

### 1. authentik

1. **Applications → Providers → Create → OAuth2/OpenID Provider**
   * Client type: *Confidential*
   * Redirect URI: `https://sign.example.com/auth/callback` (your `PUBLIC_URL` + `/auth/callback`)
   * Scopes: keep the defaults `openid`, `email`, `profile`. authentik's default profile mapping already includes the `groups` claim.
2. **Applications → Create** an application using this provider, with slug `pdfsign`.
3. Create two groups and add your users to them:
   * `pdfsign-admins` – everything, including API tokens, DocuSeal import, export to disk and deleting data
   * `pdfsign-staff` – templates, documents, people, sending links, ZIP export
   Users in neither group are refused after login. To use other names, set `OIDC_ADMIN_GROUP` / `OIDC_STAFF_GROUP`.
4. Put the values into `.env`:
   ```
   OIDC_ISSUER=https://auth.example.com/application/o/pdfsign/
   OIDC_CLIENT_ID=...
   OIDC_CLIENT_SECRET=...
   ```

Roles are re-read from authentik on every login. Sessions last 12 hours.

### 2. Discord bot

1. Create an application and bot at <https://discord.com/developers/applications>. No privileged intents are needed. Invite it with the `bot` and `applications.commands` scopes.
2. Log into pdfsign as an admin. Go to **API & export → Generate token** and put the token in `PDFSIGN_API_KEY`.
3. Fill in `DISCORD_TOKEN`, `DISCORD_CLIENT_ID`, `DISCORD_GUILD_ID` (recommended: commands show up instantly) and the template slugs `EVENT_TEMPLATE` / `EQUIPMENT_TEMPLATE`.
4. `docker compose up -d bot`. Slash commands are registered on startup.

| Command | Who | What |
| --- | --- | --- |
| `/register name zip` | everyone | Register or change your full name (checked when opening a link) and zip code |
| `/signup [event]` | everyone | Checks for a valid waiver. If there is none, you get a signing link |
| *"Interested" on a Discord scheduled event* | everyone | Same as `/signup`, delivered by DM. Staff are notified in `NOTIFY_CHANNEL_ID` if DMs are closed |
| `/equipment` | everyone | Opens your equipment form again to add items |
| `/documents` | everyone | Your documents, their status and links |
| `/handout user [items]` | staff | DMs the recipient their equipment form (asks them to register first if needed) |
| `/docs status user` / `/docs send user template` | staff | Shows a member's documents / sends a link for any template |
| `/link-account old_id new_user` | staff | Moves all data of a lost Discord account to the new one |

Staff commands require the *Manage Server* permission. If `ADMIN_ROLE_IDS` is set, they require one of those roles instead, and *Manage Server* alone no longer counts.

### Setting up the two use cases

**Event waiver:** create a template with slug `event-waiver` and set *Valid for* to 365 days. Use one required section with the fields Name (autofill: registered name), Date and Signature.

**Equipment:** create a template with slug `equipment` and leave validity empty (forever). Use a required section *Recipient* (name, date, signature). Then build one optional section for the first row: item (*Select* with your fixed equipment list), quantity (number), date and signature. Use **Sections → Repeat below…** to copy that row down the page. On the first hand-out the recipient signs the recipient section plus row 1. On later hand-outs they open the same link (via `/equipment` or the DM from `/handout`), fill in the next free row and sign it. Signed rows cannot be changed.

### General links / QR codes

In the template editor, **General links & QR codes → Create link**, then **QR** → *Print poster*, *PNG* or *SVG*. Links can be disabled or given an expiry date; signed documents stay valid either way. Opening a link:

1. The signer enters first and last name.
2. Nobody with that name yet: they must enter their zip code, and a new person is created.
3. Exactly one person with that name: they continue with that person.
4. Several people with that name: the zip code picks the right one. An unknown zip code creates a new person. If one of the namesakes has no zip on file, the signer is asked to get staff help rather than the app guessing (the People page marks these with *same name · no zip!*).
5. Valid signature already present: "You're all set". Otherwise they sign on the spot. On a shared tablet, *Done — next person* resets the page.

Identification is limited to 100 requests per IP per 10 minutes, enough for 50 first-time signers using one request for their name and one for their zip code. General links are convenient but weaker than personal links: anyone who knows a name can see whether that person has a valid signature, and can sign that person's still-open document. Use personal Discord links where that matters (e.g. equipment).

### Backups

Admins configure backups on the **Backups** page: folder, first run time, interval (hours or days) and how many to keep. With Docker, set `BACKUP_HOST_DIR` in `.env` (mounted at `/backups`) and `TZ` for the schedule's timezone.

The backup folder must be writable by `PDFSIGN_UID`/`PDFSIGN_GID`. Backups cover the database and the template PDFs. Exports and DocuSeal originals in `./export` are not included.

Each run waits for in-flight requests to finish and holds new state-changing requests for the moment of the snapshot. They are delayed, not rejected. It then checkpoints the WAL and runs `VACUUM INTO`, which captures every user and signing session. Template PDFs are copied too, then the app resumes. The pause depends on the database size. The Backups page shows the last one. The snapshot is checked (SQLite integrity, all template PDFs present) before `backup-YYYYMMDD-HHMMSS.zip` is written. A manual run does not move the schedule. Day-based intervals stay on their days and start time across DST changes.

CLI: `pdfsign backup`. It runs in its own process and does not pause a running server. The database copy is still consistent. If a template is deleted during the copy, the check fails the backup and you run it again.

Restore:

```bash
docker compose stop pdfsign
docker compose run --rm pdfsign pdfsign restore /backups/backup-YYYYMMDD-HHMMSS.zip --force
docker compose start pdfsign
```

The archive is extracted into a staging folder in `DATA_DIR` and checked first. A broken archive leaves the current data untouched. The current database and template folder are then moved to `DATA_DIR/.pre-restore-<timestamp>`, and moved back if installing the restored copy fails. Delete that folder once the restored data is verified.

### Import from DocuSeal

Go to **DocuSeal import** (admin only) and enter the API URL and API key:

* `https://api.docuseal.com` or `https://api.docuseal.eu` for the cloud
* `https://<host>/api` for self-hosted DocuSeal

Pick templates and import them. How things map:

* Multi-document templates are merged into one PDF.
* Each DocuSeal submitter role becomes a section.
* Field types `text`, `number`, `date`, `select`/`radio`, `checkbox` (drawn as "X"), and `signature`/`initials`/`image`/`stamp` are imported. Other types are reported and skipped.
* A submitter's `external_id` (or `metadata.discord_id`) that is a Discord ID is linked automatically. Other people can be linked under **People**.
* Only submissions that every submitter has completed are imported. Partly completed ones are skipped and picked up by a later import.
* Each submission (person, values, signing rounds) is written as one unit, so an interrupted import can simply be repeated. Importing again skips anything already imported. Imported IDs are qualified by the DocuSeal host, so two instances don't collide.
* The API key is only sent to the configured DocuSeal host, not to other hosts that file URLs point to.

Then click **Recreate & export all PDFs to disk**.

### Export / backup

* UI: Dashboard or **API & export** → *Export to disk* (admin) or *Download ZIP*. Documents that fail to render are reported. In the ZIP they are listed in `FAILED.txt`.
* CLI: `docker compose exec pdfsign pdfsign export [--out /export/somewhere]`. It exits non-zero if any document failed.
* A disk export writes the current documents and a new `index.csv`. It does not delete files from earlier exports, such as revoked documents or files under an old name.
* Back up `./data` (SQLite database + template PDFs). `./export` holds the generated PDFs.

## Bot / integration API

All endpoints need `Authorization: Bearer <token>`.

```
GET  /api/v1/templates
GET  /api/v1/persons/{discord_id}                          person, linked accounts, documents
PUT  /api/v1/persons/{discord_id}        {name, username, zip}  register / rename
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
| `DATA_DIR` / `EXPORT_DIR` | `/data` / `/export` | |
| `MAX_NAME_ATTEMPTS` | `10` | Wrong names before a link is locked. Staff can unlock it or issue a new link |
| `NO_AUDIT_PAGE` | unset | Set to `true` to leave out the audit page |
| `BACKUP_DIR` | `/backups` | Default backup folder inside the container (compose mounts `BACKUP_HOST_DIR` there) |
| `TZ` | `UTC` | Timezone for backup schedules |
| `TRUSTED_PROXIES` | loopback + private networks | Comma-separated addresses / CIDRs whose `X-Forwarded-For` is believed (QR rate limit, audit IP). Empty: never |

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
