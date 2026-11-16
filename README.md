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
- SQLite schema v3 with 19 tables: the v1 core (business, branch, device,
  users, auth sessions, server settings, operation log, device cursors, sync
  anomalies, audit events), master-data projections, and the order projections
  (`restaurant_order`, `order_line`, and `order_event`).
- PIN login with Argon2 hashes, hashed bearer tokens, `HttpOnly` +
  `SameSite=Strict` web session cookies, and CSRF validation on logout.
- Append-only operation log with idempotent push by operation ID and ordered
  pull by server sequence. Accepted operations are projected into the
  master-data and order read models; invalid transitions are recorded as
  anomalies instead of being applied.
- Snapshot linking: an empty server accepts a desktop snapshot and issues a
  device token; a non-empty server rejects the link with a conflict. Servers
  are never merged silently.
- Spanish mobile web app for tables, takeout, order detail, rounds, kitchen,
  and menu, with an online/offline banner, a live status fragment, and the
  sold-out toggle.
- Order flow: open a free table or create a takeout order, add lines and notes,
  send the pending round, void a line with a reason, mark lines or whole orders
  ready and served, and refresh the kitchen board with HTMX polling every five
  seconds.
- Role checks: `admin`, `cashier`, and `waiter` can open orders, add lines,
  send rounds, void, and serve; `admin` and `kitchen` can mark ready; every role
  can view the tables, takeout list, order detail, menu, and kitchen board.
- 37 tests, including order projection folding, duplicate-table and invalid
  transition anomalies, the full waiter/kitchen web flow, takeout creation, and
  CSRF/role rejection.

The cashier screen is built in the next phase.

## Routes

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/` | Redirects to `/app` or `/login` |
| `GET` | `/login` | PIN login page |
| `POST` | `/login` | Validates the PIN and sets the session cookie |
| `POST` | `/logout` | Revokes the session; requires the CSRF token |
| `GET` | `/app` | Waiter/kitchen web shell |
| `GET` | `/app/tables` | Table grid with open orders |
| `POST` | `/app/tables/{id}/open` | Opens a dine-in order for a free table |
| `GET`, `POST` | `/app/takeout` | Lists open takeout orders and creates one |
| `GET` | `/app/orders/{id}` | Order detail with rounds and line actions |
| `POST` | `/app/orders/{id}/lines` | Adds a pending line to the current round |
| `POST` | `/app/orders/{id}/send` | Sends the pending round to the kitchen |
| `POST` | `/app/orders/{id}/ready` | Marks every sent line ready |
| `POST` | `/app/orders/{id}/served` | Marks every sent or ready line served |
| `POST` | `/app/orders/{id}/lines/{line_id}/void` | Voids a line with a reason |
| `POST` | `/app/orders/{id}/lines/{line_id}/ready` | Marks one sent line ready |
| `POST` | `/app/orders/{id}/lines/{line_id}/served` | Marks one line served |
| `GET` | `/app/kitchen` | Kitchen board |
| `GET` | `/app/kitchen/board` | HTMX kitchen-board fragment |
| `GET` | `/app/menu` | Menu read view with the sold-out toggle |
| `POST` | `/app/menu/items/{id}/availability` | Marks an item available or sold out; requires the CSRF token |
| `GET` | `/app/status` | HTMX status fragment |
| `GET` | `/static/app.css` | Embedded stylesheet |
| `GET` | `/static/htmx.min.js` | Embedded HTMX 2.0.4 |
| `GET` | `/api/health` | Liveness and database check |
| `POST` | `/api/login` | Exchanges a PIN for an API bearer token |
| `POST` | `/api/link` | Imports a desktop snapshot into an empty server and issues a device token |
| `POST` | `/api/sync/push` | Appends operations; requires a bearer token |
| `GET` | `/api/sync/pull` | Pulls operations after `since`; requires a bearer token |

## Linking

`POST /api/link` receives the server admin user, its PIN, and the desktop
snapshot. The server validates the credentials and imports the snapshot only
when its own data is empty; otherwise it returns `409 Conflict`. On success it
returns the device ID, a device token, the business and branch IDs, and the
last sequence.

After linking, the desktop admin replaces the seed `Admin` user, so the web app
logs in with the desktop credentials. The device token authenticates push and
pull requests, and the server only accepts operations authored by that device.

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
