# nexo-server

Local server and web app for **Nexo Restaurantes**. One local server is the
system of record and the web app is the only client for admin, cashier, waiters,
and kitchen. The archived desktop and Flutter projects are not part of the daily
flow.

This is an **independent project**. It consumes `nexo-core` as a local path
dependency and never embeds the desktop application.

## Role in the product

```
Admin / cashier / waiter / kitchen browsers
                  │ HTTP on the restaurant LAN
                  ▼
             nexo-server
          (SQLite, operations,
           projections, web app)
```

- The server runs on the cashier PC or a dedicated mini-PC. It does not need
  internet.
- A cash register is a browser pointing to the server, not a second
  installation. Multiple registers share the menu, orders, sales, and reports;
  each browser keeps its own cash session.
- The web app is online-only with respect to the server and shows an offline
  message when the LAN is unavailable.
- The legacy device sync API (`/api/link`, `/api/sync/push`, `/api/sync/pull`)
  remains only for a cold fallback with the archived desktop. New features are
  not built on it.

## Current state

- Axum 0.8 + SQLx 0.9 (SQLite) + Askama 0.16 + HTMX, with mobile-first CSS and
  no Node build.
- SQLite schema v6 with 25 tables: core identity and sync, master-data
  projections, order projections, and billing projections. Migration `0006`
  adds `payment.tendered_minor` and `payment.change_minor`.
- PIN login with Argon2 hashes, hashed bearer tokens, `HttpOnly` +
  `SameSite=Strict` web session cookies, CSRF validation on logout and every
  mutating form, and a persistent login throttle shared by web login, API
  login, and snapshot linking. Sessions stop working immediately when the user
  is deactivated.
- Append-only operation log with idempotent push by operation ID and ordered
  pull by server sequence. Web mutations append their operations and apply the
  projections in one transaction.
- Snapshot linking: an empty server accepts an archived-desktop snapshot and
  issues a device token; a non-empty server rejects the link with a conflict.
  Servers are never merged silently.
- Web app for tables, takeout, order detail, rounds, kitchen, and menu, with an
  online/offline banner, live polling, and the sold-out toggle. The shell fits
  all tabs at 360 px, uses 48 px tap targets, marks the active tab with
  `aria-current="page"`, and serves an embedded manifest, icons, and Apple touch
  metadata for an HTTP home-screen shortcut. Each kitchen browser can filter the
  board by menu category through the `nexo_kitchen_categories` cookie; mixed
  orders show each kitchen only its own lines, uncategorized items stay visible
  everywhere, and **Todo listo** only marks the visible lines.
- Web cashier: finds billable orders, charges one or more payments in USD, VES,
  or a mix, supports tendered/change and references, records the sale and cash
  movements atomically, frees the table, and shows a printable receipt.
- Web cash sessions: open, cash in/out, close, and expected/counted/difference
  amounts per currency. Each browser has its own persistent web device and cash
  session.
- Web reports: daily sales, included IVA, payments by method and currency, top
  items, cash closures, CSV download, and browser print/PDF.
- Web admin: business name, exchange rate, optional IVA, menu categories and
  items, tables, users and PINs, payment methods, and a consistent SQLite backup
  download.
- Order flow: open a free table or create a takeout order, add lines and notes,
  send the pending round, void a line, mark lines or whole orders ready and
  served, cancel an order with a reason, and refresh the order status, tables,
  takeout list, cashier, and kitchen board with HTMX polling.
- Role checks: `admin`, `cashier`, and `waiter` can open orders, add lines,
  send rounds, void, and serve; `admin` and `cashier` can bill, run cash, see
  reports, and cancel orders; `admin` and `kitchen` can mark ready; `admin` can
  use the admin module. The navigation hides modules the role cannot use.
- 75 tests, including projection folding, billing arithmetic and IVA, the
  waiter/kitchen/cashier web flow, mixed payments and change, cash sessions,
  reports, admin writes and backup, login throttling, permissions, CSRF,
  kitchen category filtering, and data/session/device persistence across a
  server restart.

## Routes

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/` | Redirects to `/app` or `/login` |
| `GET`, `POST` | `/login` | PIN login |
| `POST` | `/logout` | Revokes the session; requires the CSRF token |
| `GET` | `/app` | Module home |
| `GET` | `/app/tables` | Table grid with open orders; polls every 5 s |
| `POST` | `/app/tables/{id}/open` | Opens a dine-in order for a free table |
| `GET`, `POST` | `/app/takeout` | Lists open takeout orders and creates one; polls every 5 s |
| `GET` | `/app/orders/{id}` | Order detail with rounds and line actions |
| `GET` | `/app/orders/{id}/status` | Live order status fragment; polls every 5 s |
| `POST` | `/app/orders/{id}/lines` | Adds a pending line to the current round |
| `POST` | `/app/orders/{id}/send` | Sends the pending round to the kitchen |
| `POST` | `/app/orders/{id}/ready` | Marks every sent line ready |
| `POST` | `/app/orders/{id}/served` | Marks every sent or ready line served |
| `POST` | `/app/orders/{id}/cancel` | Cancels an open order with a reason |
| `POST` | `/app/orders/{id}/lines/{line_id}/void` | Voids a line with a reason |
| `POST` | `/app/orders/{id}/lines/{line_id}/ready` | Marks one sent line ready |
| `POST` | `/app/orders/{id}/lines/{line_id}/served` | Marks one line served |
| `GET`, `POST` | `/app/orders/{id}/bill` | Billing form and sale confirmation |
| `GET` | `/app/sales/{id}` | Printable receipt |
| `GET` | `/app/cashier` | Cashier dashboard; polls every 5 s |
| `GET` | `/app/cash` | Current cash session |
| `POST` | `/app/cash/open` | Opens a cash session |
| `POST` | `/app/cash/movements` | Records a cash in/out movement |
| `POST` | `/app/cash/close` | Closes the session with counted amounts |
| `GET` | `/app/reports` | Daily report |
| `GET` | `/app/reports.csv` | Daily sales CSV (UTF-8 with BOM) |
| `GET` | `/app/admin` | Administration page |
| `POST` | `/app/admin/business` | Business name, exchange rate, and IVA |
| `POST` | `/app/admin/categories` | Creates or updates a menu category |
| `POST` | `/app/admin/items` | Creates or updates a menu item |
| `POST` | `/app/admin/tables` | Creates or updates a dining table |
| `POST` | `/app/admin/users` | Creates or updates a user and PIN |
| `POST` | `/app/admin/payment-methods` | Creates or updates a payment method |
| `GET` | `/app/admin/backup` | Downloads a consistent SQLite copy |
| `GET` | `/app/kitchen` | Kitchen board |
| `POST` | `/app/kitchen/filter` | Saves the kitchen category filter for this browser |
| `GET` | `/app/kitchen/board` | HTMX kitchen-board fragment |
| `GET` | `/app/menu` | Menu read view with the sold-out toggle |
| `POST` | `/app/menu/items/{id}/availability` | Marks an item available or sold out |
| `GET` | `/app/status` | HTMX status fragment |
| `GET` | `/static/*` | Embedded CSS, HTMX, manifest, and icons |
| `GET` | `/api/health` | Liveness and database check |
| `POST` | `/api/login` | Exchanges a PIN for an API bearer token |
| `POST` | `/api/link` | Imports an archived-desktop snapshot into an empty server |
| `POST` | `/api/sync/push` | Appends operations; requires a bearer token |
| `GET` | `/api/sync/pull` | Pulls operations after `since`; requires a bearer token |

## First start

1. Build the release binary:

   ```sh
   cargo build --release
   ```

2. Start the server once with an admin PIN:

   ```sh
   NEXO_SERVER_DB='sqlite:///var/lib/nexo-server/nexo-server.db?mode=rwc' \
   NEXO_ADMIN_PIN=1234 \
   NEXO_BUSINESS_NAME='Mi Restaurante' \
   ./target/release/nexo-server
   ```

3. Open `http://<server-ip>:8080` and log in as `Admin` with that PIN.
4. In **Administración**, set the business name, exchange rate, optional IVA,
   menu, tables, users, and payment methods.
5. Open **Caja**, open the first cash session, and start service.

The seed runs only when the database has no users. After creating the real
admin users, remove `NEXO_ADMIN_PIN` from the service environment.

## Configuration

| Variable | Default | Notes |
|---|---|---|
| `NEXO_SERVER_DB` | `sqlite://nexo-server.db?mode=rwc` | SQLite URL |
| `NEXO_SERVER_ADDR` | `0.0.0.0:8080` | Listen address |
| `NEXO_ADMIN_PIN` | unset | Seeds the first admin only when no users exist |
| `NEXO_BUSINESS_NAME` | `Nexo Restaurantes` | Used when seeding |
| `NEXO_SESSION_HOURS` | `12` | Session lifetime, 1 to 168 hours |

## Deployment

The MVP is deployed as a release binary on the cashier PC or a small mini-PC,
not as a container or an OS package. Static assets are embedded, so no Node
build or asset directory is needed at runtime.

### Dedicated service (Linux example)

Create a system user and data directory:

```sh
sudo useradd --system --create-home \
  --home-dir /var/lib/nexo-server \
  --shell /usr/sbin/nologin nexo
sudo install -m 755 target/release/nexo-server /usr/local/bin/nexo-server
sudo install -d -o nexo -g nexo /var/lib/nexo-server
```

Create `/etc/systemd/system/nexo-server.service`:

```ini
[Unit]
Description=Nexo Restaurantes local server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=nexo
Group=nexo
WorkingDirectory=/var/lib/nexo-server
Environment=NEXO_SERVER_DB=sqlite:///var/lib/nexo-server/nexo-server.db?mode=rwc
Environment=NEXO_SERVER_ADDR=0.0.0.0:8080
Environment=NEXO_SESSION_HOURS=12
# First start only, then remove this line:
# Environment=NEXO_ADMIN_PIN=1234
ExecStart=/usr/local/bin/nexo-server
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

Enable and start it:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now nexo-server
sudo systemctl status nexo-server
sudo journalctl -u nexo-server -f
```

### Network

- Give the server machine a fixed IP address or a DHCP reservation.
- Allow TCP 8080 only from the restaurant LAN:

  ```sh
  sudo ufw allow from 192.168.1.0/24 to any port 8080 proto tcp
  ```

- Do not forward the port on the router or expose the server to the internet.
  The MVP has no TLS or reverse proxy; local HTTPS is deferred.
- Verify from another device with `curl http://<server-ip>:8080/api/health`.

### Updating

1. Back up the server database.
2. Stop the service.
3. Replace the binary.
4. Start the service and check `/api/health`.

Migrations run at startup. If a migration fails, the service does not accept
requests; restore the backup before retrying.

## Backup and restore

The server database holds the operation log, projections, users, sessions, and
device tokens. It is the system of record, so back it up daily.

From the web app, an admin can press **Descargar respaldo** in
**Administración**. The server uses SQLite `VACUUM INTO`, so the download is a
consistent copy while the server keeps running.

With the service running, the equivalent command is:

```sh
sudo mkdir -p /var/backups/nexo-server
sudo sqlite3 /var/lib/nexo-server/nexo-server.db \
  ".backup '/var/backups/nexo-server/server-$(date +%F).db'"
```

With the service stopped, a file copy is safe. Also back up the systemd unit
and any environment file. To restore, stop the service, move the current
database aside, copy the backup into place, remove stale `-wal` and `-shm`
files, and start the service. Never merge two server databases by hand.

See [`USER_GUIDE.md`](../USER_GUIDE.md) for the operator routine.

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
`nexo-restaurant/examples/server_e2e.rs` against it, and verifies the legacy
cold-fallback slice. It is kept as a regression check for the archived desktop;
the daily flow no longer depends on it.

```sh
./scripts/restaurant-e2e.sh
```

It needs `curl`, `jq`, and `sqlite3`. Set `NEXO_RESTAURANT_DIR` if the archived
desktop is elsewhere, `NEXO_E2E_PORT` to change the port, or `NEXO_E2E_KEEP=1`
to keep the temporary databases.

## Security notes

- PINs are hashed with Argon2; tokens are stored as SHA-256 hashes, never in
  plain text.
- Failed PIN attempts are counted per normalized user name in a persistent
  one-hour window shared by web login, API login, and snapshot linking. Locks
  are progressive: 5 failures lock for 1 minute, 10 for 5 minutes, 15 for 15
  minutes, and 20 or more for 1 hour. A successful login clears the counter.
  The web login page and API return `429 Too Many Requests` with the actual
  remaining seconds in `Retry-After`.
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
