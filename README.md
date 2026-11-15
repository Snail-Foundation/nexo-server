# nexo-server

Local server for **Nexo Restaurantes**: synchronization hub, PIN authentication,
and the mobile web app used by waiters and kitchen staff.

This is an **independent project**. It consumes `nexo-core` as a local path
dependency and never embeds the desktop application.

## Role in the product

The desktop is autonomous: it bills already-synced orders without the server.
The local server is required only by waiter and kitchen devices, which are
mobile web browsers on the same Wi-Fi network.

```
Waiter phone ─┐
Kitchen screen ├─ HTTP ──> nexo-server ── sync ──> nexo-restaurant desktop
Cashier browser┘
```

The server runs on the cashier PC or a dedicated mini-PC. It is the same binary
and the same URLs in both cases.

## Current state

- Axum 0.8 + SQLx 0.9 (SQLite) + Askama 0.16 + HTMX, with mobile-first CSS and
  no Node build.
- SQLite schema v1 with 10 tables: business, branch, device, users, auth
  sessions, server settings, operation log, device cursors, sync anomalies, and
  audit events.
- PIN login with Argon2 hashes, hashed bearer tokens, `HttpOnly` +
  `SameSite=Strict` web session cookies, and CSRF validation on logout.
- Append-only operation log with idempotent push by operation ID and ordered
  pull by server sequence.
- Spanish login and web shell with an online/offline banner and a live status
  fragment.

The ordering, kitchen, and cashier screens are built in the next phases.

## Routes

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/` | Redirects to `/app` or `/login` |
| `GET` | `/login` | PIN login page |
| `POST` | `/login` | Validates the PIN and sets the session cookie |
| `POST` | `/logout` | Revokes the session; requires the CSRF token |
| `GET` | `/app` | Waiter/kitchen web shell |
| `GET` | `/app/status` | HTMX status fragment |
| `GET` | `/static/app.css` | Embedded stylesheet |
| `GET` | `/static/htmx.min.js` | Embedded HTMX 2.0.4 |
| `GET` | `/api/health` | Liveness and database check |
| `POST` | `/api/login` | Exchanges a PIN for an API bearer token |
| `POST` | `/api/sync/push` | Appends operations; requires a bearer token |
| `GET` | `/api/sync/pull` | Pulls operations after `since`; requires a bearer token |

## Configuration

| Variable | Default | Notes |
|---|---|---|
| `NEXO_SERVER_DB` | `sqlite://nexo-server.db?mode=rwc` | SQLite URL |
| `NEXO_SERVER_ADDR` | `0.0.0.0:8080` | Listen address |
| `NEXO_ADMIN_PIN` | unset | Seeds the first admin only when no users exist |
| `NEXO_BUSINESS_NAME` | `Nexo Restaurantes` | Used when seeding |
| `NEXO_SESSION_HOURS` | `12` | Session lifetime, 1 to 168 hours |

The seed creates one business, a default branch named `Principal`, a server
device, and an `Admin` user with the `admin` role. Without `NEXO_ADMIN_PIN` and
with an empty database, web and API login stay disabled.

## Development

```sh
cargo fmt
cargo test
cargo clippy --all-targets
```

Run locally:

```sh
NEXO_ADMIN_PIN=1234 cargo run
```

Then open `http://localhost:8080` on the same machine or
`http://<server-ip>:8080` from a phone on the same network.

## Security notes

- PINs are hashed with Argon2; tokens are stored as SHA-256 hashes, never in
  plain text.
- Web sessions use `HttpOnly` and `SameSite=Strict` cookies. Logout requires the
  per-session CSRF token.
- API clients authenticate with `Authorization: Bearer <token>`.
- The MVP serves plain HTTP on the local network. Full PWA installability needs
  HTTPS and is deferred; the current client uses a home-screen shortcut.

## License

MIT. See [`LICENSE`](./LICENSE).
