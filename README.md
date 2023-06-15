# clair-operator

clair-operator is a *beta* operator to run a clair installation.

## Restrictions

- A PostgresQL database is required.
  The operator does not provision one automatically.
- The minimum Clair version supported is `v4.7.0`/`nightly-2023-06-16`.
- There is no "combo" mode support.
