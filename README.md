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
- SQLite schema v5 with 25 tables: the v1 core (business, branch, device,
  users, auth sessions, login throttling, server settings, operation log,
  device cursors, sync anomalies, audit events), master-data projections, the
  order projections (`restaurant_order`, `order_line`, and `order_event`), and
  the billing projections (`sale`, `sale_line`, `payment`, `cash_session`, and
  `cash_movement`).
- PIN login with Argon2 hashes, hashed bearer tokens, `HttpOnly` +
  `SameSite=Strict` web session cookies, CSRF validation on logout and every
  mutating form, and a persistent login throttle shared by web login, API
  login, and snapshot linking. Sessions stop working immediately when the user
  is deactivated.
- Append-only operation log with idempotent push by operation ID and ordered
  pull by server sequence. Accepted operations are projected into the
  master-data, order, sale, payment, and cash read models; invalid transitions
  are recorded as anomalies instead of being applied.
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
- Billing projections: `sale_created` inserts the sale, its lines, and its
  payments; `order_billed` is commutative and transitions the order without
  requiring the sale row; `payment_recorded`, `cash_session_opened`,
  `cash_movement_recorded`, and `cash_session_closed` are idempotent; and
  `sale_cancelled` records an audit event. Billed orders appear as "Cobrado" in
  the web app.
- Role checks: `admin`, `cashier`, and `waiter` can open orders, add lines,
  send rounds, void, and serve; `admin` and `kitchen` can mark ready. Page
  access follows the role: tables, takeout, and order detail are for
  `admin`/`cashier`/`waiter`; the kitchen board is for `admin`/`kitchen`; the
  menu and sold-out toggle are available to every role. The navigation hides
  modules the current role cannot use.
- 50 tests, including order and billing projection folding, commutative
  `order_billed`, rejected billing of a cancelled order, duplicate-table and
  invalid transition anomalies, the full waiter/kitchen web flow, takeout
  creation, login throttling across web and API, inactive-user session
  rejection, page permissions, CSRF/role rejection, and data/session/device
  persistence across a server restart.

Cashier billing remains desktop-first: the web app displays billed orders but
does not charge them.

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

## Restart behavior

The SQLite database stores the operation log, projections, auth sessions, and
device tokens, so they survive a server restart. Startup applies any pending
migrations before accepting requests. `Ctrl+C` and `SIGTERM` trigger a graceful
shutdown: in-flight requests finish, then the process exits. The web app keeps
its session cookie, and the desktop sync client resumes from its saved cursor
after the server comes back.

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

## Cross-project integration check

`scripts/restaurant-e2e.sh` starts a throwaway server, runs
`nexo-restaurant/examples/server_e2e.rs` against it, and verifies the whole
slice:

- The desktop creates an installation, menu, table, cash session, dine-in
  order, and cash sale, then links and syncs with the server.
- The server projects the order as billed, the sale and payment, the open cash
  session, and the sale cash movement without synchronization anomalies.
- The waiter web app logs in with the desktop admin PIN and shows the billed
  order as `Cobrado`.

Run it from this checkout with `nexo-restaurant` next to it:

```sh
./scripts/restaurant-e2e.sh
```

It needs `curl`, `jq`, and `sqlite3`. Set `NEXO_RESTAURANT_DIR` if the desktop
is elsewhere, `NEXO_E2E_PORT` to change the port, or `NEXO_E2E_KEEP=1` to keep
the temporary databases. The projects stay independent: the harness only uses
the public HTTP protocol and reads both SQLite databases for assertions.

## Security notes

- PINs are hashed with Argon2; tokens are stored as SHA-256 hashes, never in
  plain text.
- Failed PIN attempts are counted per normalized user name in a persistent
  fixed window. After 5 failures in 15 minutes the user is locked for 15
  minutes. Web login, API login, and snapshot linking share the same counter,
  so switching channels does not reset it. The web login page and API return
  `429 Too Many Requests` with `Retry-After`.
- Web sessions use `HttpOnly` and `SameSite=Strict` cookies. Logout requires the
  per-session CSRF token, and CSRF tokens are compared in constant time.
- Sessions are checked against the user's active flag on every request, so
  deactivating a user invalidates existing web and API sessions.
- Page handlers enforce roles in addition to the mutation handlers, and the
  navigation only shows modules the current role can use.
- API clients authenticate with `Authorization: Bearer <token>`.
- The MVP serves plain HTTP on the local network. Full PWA installability needs
  HTTPS and is deferred; the current client uses a home-screen shortcut.

## License

MIT. See [`LICENSE`](./LICENSE).
