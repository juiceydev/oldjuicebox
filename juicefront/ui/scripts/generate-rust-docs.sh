#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_DIR="$(cd "$(dirname "$0")/../../.." && pwd)"
PUBLIC_DIR="$(cd "$(dirname "$0")/../public" && pwd)"

echo "-> Generating Rust documentation..."
cargo doc --no-deps --workspace --quiet 2>"$WORKSPACE_DIR/target/doc-gen-warnings.txt" || true

if [ -s "$WORKSPACE_DIR/target/doc-gen-warnings.txt" ]; then
    echo "! rustdoc warnings:"
    cat "$WORKSPACE_DIR/target/doc-gen-warnings.txt"
fi

echo "-> Copying docs to public/rustdoc/"
rm -rf "$PUBLIC_DIR/rustdoc"
cp -a "$WORKSPACE_DIR/target/doc" "$PUBLIC_DIR/rustdoc"
echo "[ok] Rust documentation generated"
