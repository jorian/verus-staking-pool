# Verus Staking Pool

This is the open source staking pool for Verus and its PBaaS chains.

## Install

1. Clone this repo
2. Add a `config` directory in root
3. In this directory, create `base.toml`. These contain the settings regardless of your application environment.
4. Depending on your environment, create one of `local.toml`, `development.toml`, `test.toml` or `production.toml` next to the just created `base.toml`.
5. Install Rust: https://rustup.rs
6. Install docker
7. Run postgres and NATS docker containers
8. Install SQLX: `cargo install sqlx-cli`
9. Set up postgres database
10. `cat /dev/urandom | tr -dc 'a-zA-Z0-9' | fold -w 32 | head -n 1`
