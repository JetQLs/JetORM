set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# List all available commands.
default:
    @just --list

# Build the whole workspace
build:
    cargo build --workspace

# Unit and integration tests (no database required)
test:
    cargo xtask test

# rustfmt check + clippy with warnings denied
lint:
    cargo xtask lint

# Start the manual development database (docker-compose.test.yml)
db-up:
    cargo xtask db-up

# Stop the development database and discard its data
db-down:
    cargo xtask db-down

# Live E2E tests; each starts its own disposable database container (needs Docker)
test-live:
    cargo xtask test-live

# Query-build benchmarks: JetORM vs SeaORM vs Diesel (criterion)
bench:
    cargo xtask bench

# Everything CI runs without a database
ci:
    cargo xtask ci
