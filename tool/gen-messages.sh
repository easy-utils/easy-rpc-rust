#!/usr/bin/env bash
# Regenerate vendored prost messages + pbjson serde impls for a proto.
#
# The conformance build is offline: prost output is committed under
# src/<pkg-path>/. This script regenerates it via a scratch crate that depends
# on prost-build + pbjson-build (which resolve from crates.io/cache), then
# copies both files into the vendored directory.
#
# Usage: tool/gen-messages.sh <proto-root> <proto-rel-path> <dest-dir> <rust-pkg>
set -euo pipefail
ROOT="$1"; REL="$2"; DEST="$3"; PKG="$4"
GEN=/tmp/opencode/rustgen
mkdir -p "$GEN/src" "$GEN/proto"
[ -f "$GEN/Cargo.toml" ] || cat > "$GEN/Cargo.toml" <<'TOML'
[package]
name = "easyrpc-rustgen"
version = "0.1.0"
edition = "2021"
[build-dependencies]
prost-build = "0.13"
pbjson-build = "0.9"
TOML
[ -f "$GEN/src/main.rs" ] || echo 'fn main(){}' > "$GEN/src/main.rs"
cat > "$GEN/build.rs" <<'RS'
fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ws = std::env::var("EASYRPC_PROTO_ROOT").unwrap();
    let rel = std::env::var("EASYRPC_PROTO").unwrap();
    let pkg = std::env::var("EASYRPC_PKG").unwrap();
    let desc = out.join("ds.bin");
    prost_build::Config::new()
        .file_descriptor_set_path(&desc)
        .compile_protos(&[format!("{ws}/{rel}")], &[ws.clone()])
        .unwrap();
    let bytes = std::fs::read(&desc).unwrap();
    pbjson_build::Builder::new()
        .register_descriptors(&bytes).unwrap()
        .ignore_unknown_fields()
        .out_dir(&out)
        .build(&[pkg.as_str()]).unwrap();
}
RS
( cd "$GEN" && EASYRPC_PROTO_ROOT="$ROOT" EASYRPC_PROTO="$REL" EASYRPC_PKG="$PKG" cargo build -q )
OUT=$(find "$GEN/target/debug/build" -maxdepth 3 -path "*/easyrpc-rustgen-*/out" -type d | head -1)
B="$(echo "$PKG" | sed 's/^\.//')"
cp "$OUT/$B.rs" "$DEST/$B.rs"
cp "$OUT/$B.serde.rs" "$DEST/$B.serde.rs"
echo "regenerated $DEST/$B.rs + .serde.rs"
