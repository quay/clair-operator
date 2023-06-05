# clair-operator

clair-operator is a *beta* operator to run a clair installation.

To take it for a spin, run `cargo xtask demo` and look at the manifests in `config/samples`.

## Building

To build, the project needs:

- rust toolchain
- go toolchain
- clang
- openssl-devel

Some `xtask` subcommands additionally need:

- kubectl
- kustomize
- kind

## Restrictions

- A PostgresQL database is required.
  The operator does not provision one automatically.
- The minimum Clair version supported is `v4.7.0`/`nightly-2023-06-16`.
- There is no "combo" mode support.
