# AGENTS.md — nexo-server

## Project

Local server and web app for Nexo Restaurantes. One server per business is the
system of record; the web app is the only client for admin, cashier, waiters,
and kitchen. The archived desktop and Flutter projects are out of the daily
flow.

## Source of truth

`README.md` documents the public build, routes, configuration, deployment,
backup, and current state. The global Nexo plan and decision log live outside
this repository.

## Build and verify

`nexo-core` is consumed through `path = "../nexo-core"`. Clone both repositories
side by side before building:

```sh
git clone https://github.com/Snail-Foundation/nexo-core.git
git clone https://github.com/Snail-Foundation/nexo-server.git
cd nexo-server
```

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

The cross-project check is optional and requires the archived
`nexo-restaurant-legacy` repository next to this one:

```sh
./scripts/restaurant-e2e.sh
```

## Rules

- Keep repository artifacts in English; the web UI is localized, with Spanish
  as the first locale.
- The server is local and must keep operating without internet. The web app is
  online-only with respect to the server and shows an offline message.
- Sales, payments, cash movements, and order events are append-only. Never
  discard a paid sale.
- Master data is edited only by `admin` users. Billing is serialized so two
  registers cannot select the same sale number.
- The restaurant MVP has no inventory, stock, purchases, suppliers, or recipes.
- Migrations are immutable once applied. Add a new numbered migration; never
  edit an existing one.
- Do not add desktop dependencies or embed the archived desktop application.
- Kitchen printers are server-local hardware configuration, not shared
  operations. Print tickets never include prices.
