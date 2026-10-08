#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [ -f "$PROJECT_ROOT/.env" ]; then
    set -a
    . "$PROJECT_ROOT/.env"
    set +a
fi

DB_PATH="${DATABASE_PATH:?DATABASE_PATH not set -- check $PROJECT_ROOT/.env}"
JUICEBACK_BIN="${JUICEBACK_BIN:-}"

usage() {
    echo "Usage: $0 {add|remove|list} [username]"
    echo
    echo "Commands:"
    echo "  add <username>    Create a new admin user"
    echo "  remove <username> Remove an admin user"
    echo "  list              List all admin users"
    echo
    echo "Environment:"
    echo "  DATABASE_PATH  Read from $PROJECT_ROOT/.env (required)"
    echo "  JUICEBACK_BIN  Path to juiceback binary (auto-detected if unset)"
    exit 1
}

ensure_db() {
    if [ ! -f "$DB_PATH" ]; then
        echo "Error: database not found at $DB_PATH"
        echo "Start juiceback at least once to initialize the database, then try again."
        exit 1
    fi
}

hash_password() {
    local password="$1"
    local bin="$JUICEBACK_BIN"

    if [ -z "$bin" ]; then
        for p in "$PROJECT_ROOT/target/release/juiceback" "$PROJECT_ROOT/target/debug/juiceback"; do
            if [ -f "$p" ]; then bin="$p"; break; fi
        done
    fi

    if [ -n "$bin" ]; then
        echo "$password" | "$bin" hash-password --stdin
    elif command -v cargo &>/dev/null; then
        echo "Building juiceback (debug)..."
        cargo build 1>&2
        echo "$password" | "$PROJECT_ROOT/target/debug/juiceback" hash-password --stdin
    else
        echo "Error: juiceback binary not found and cargo is not available."
        echo "Build with: cargo build -p juiceback"
        exit 1
    fi
}

case "${1:-}" in
    add)
        USERNAME="${2:-}"
        if [ -z "$USERNAME" ]; then usage; fi
        ensure_db
        read -s -p "Password: " PASSWORD
        echo
        read -s -p "Confirm password: " PASSWORD2
        echo
        if [ "$PASSWORD" != "$PASSWORD2" ]; then
            echo "Error: passwords do not match"
            exit 1
        fi
        if [ -z "$PASSWORD" ]; then
            echo "Error: password cannot be empty"
            exit 1
        fi

        echo "Hashing password..."
        HASH=$(hash_password "$PASSWORD")
        sqlite3 "$DB_PATH" \
            ".parameter set :username $USERNAME" \
            ".parameter set :password_hash $HASH" \
            "INSERT INTO admins (username, password_hash) VALUES (:username, :password_hash);"
        echo "Admin '$USERNAME' added."
        ;;
    remove)
        USERNAME="${2:-}"
        if [ -z "$USERNAME" ]; then usage; fi
        ensure_db
        sqlite3 "$DB_PATH" \
            ".parameter set :username $USERNAME" \
            "DELETE FROM admins WHERE username = :username;"
        echo "Admin '$USERNAME' removed."
        ;;
    list)
        ensure_db
        sqlite3 -header "$DB_PATH" "SELECT username, created_at AS created FROM admins ORDER BY created_at ASC;"
        ;;
    *)
        usage
        ;;
esac
