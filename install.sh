#!/bin/sh
# yggdrasilhq one-shot installer — curl | sh friendly, no npm required.
#
#   curl -fsSL https://raw.githubusercontent.com/yggdrasilhq/<repo>/main/install.sh | sh
#
# Pulls the platform binary straight from the npm registry (public packages:
# the metadata and tarball are plain HTTPS, no auth, no registry tooling),
# drops it in ~/.local/bin, and verifies it runs. Prefer `ynpm install` when
# yggterm is present — ynpm keeps every yggdrasilhq binary current across the
# whole fleet, with generations and rollback.
set -eu

REPO_NAME="${YNPM_PACKAGE:-ytop}"
BIN_NAME="${YNPM_BIN:-ytop}"
SCOPE="@ygghq"
DEST="${YNPM_DEST:-$HOME/.local/bin}"

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64)  PLATFORM=linux-x64 ;;
  Linux/aarch64|Linux/arm64) PLATFORM=linux-arm64 ;;
  Darwin/x86_64) PLATFORM=darwin-x64 ;;
  Darwin/arm64)  PLATFORM=darwin-arm64 ;;
  *) echo "install.sh: no prebuilt binary for $(uname -s)/$(uname -m) (linux and darwin ship today)" >&2; exit 1 ;;
esac

command -v curl >/dev/null || { echo "install.sh: curl is required" >&2; exit 1; }

PKG="$SCOPE/$REPO_NAME-$PLATFORM"
echo ">> fetching $PKG from the npm registry"
META=$(curl -fsSL "https://registry.npmjs.org/$PKG/latest" | sed -n 's/.*"tarball":"\([^"]*\)".*/\1/p' | tail -1)
[ -n "$META" ] || { echo "install.sh: could not read the registry metadata for $PKG" >&2; exit 1; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
echo ">> downloading $META"
curl -fsSL -o "$TMP/pkg.tgz" "$META"
tar -xzf "$TMP/pkg.tgz" -C "$TMP"

mkdir -p "$DEST"
cp "$TMP/package/bin/$BIN_NAME" "$DEST/$BIN_NAME"
chmod 755 "$DEST/$BIN_NAME"

echo ">> verifying"
"$DEST/$BIN_NAME" --version

case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo ">> note: $DEST is not on your PATH — add it to your shell profile" ;;
esac
echo "installed: $DEST/$BIN_NAME"
