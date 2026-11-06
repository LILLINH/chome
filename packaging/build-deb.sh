#!/usr/bin/env bash
# build-deb.sh — đóng gói Chỏme thành file .deb (Ubuntu 24.04+ / Debian 13+).
#
# Dùng:  packaging/build-deb.sh            # tự build bản release rồi đóng gói
#        BIN=đường/dẫn/chome packaging/build-deb.sh   # đóng gói file chạy có sẵn
# Kết quả: dist/chome_<phiên bản>_amd64.deb
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
ARCH="$(dpkg --print-architecture)"
PKG="chome"
# Người bảo trì ghi trong gói; đặt biến MAINTAINER="Tên <email>" khi đóng gói nếu muốn.
MAINTAINER="${MAINTAINER:-Chỏme Maintainers <maintainers@chome.invalid>}"

if [ -z "${BIN:-}" ]; then
  cargo build --release --workspace
  BIN="target/release/chome"
fi
[ -x "$BIN" ] || { echo "Không tìm thấy file chạy: $BIN" >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

install -Dm755 "$BIN" "$STAGE/usr/bin/chome"
# Cầu nối MCP cho Claude Code / Codex / Gemini CLI.
MCP_BIN="${MCP_BIN:-$(dirname "$BIN")/chome-mcp}"
[ -x "$MCP_BIN" ] || MCP_BIN="target/release/chome-mcp"
[ -x "$MCP_BIN" ] || { echo "Không tìm thấy chome-mcp (cargo build --release --workspace)" >&2; exit 1; }
install -Dm755 "$MCP_BIN" "$STAGE/usr/bin/chome-mcp"
strip --strip-unneeded "$STAGE/usr/bin/chome" 2>/dev/null || true
# Khóa công khai của kho APT (để apt kiểm tra chữ ký kho cập nhật).
install -Dm644 packaging/chome-archive-keyring.gpg "$STAGE/usr/share/keyrings/chome-archive-keyring.gpg"

# Icon nhiều kích cỡ (tạo từ assets/icon.png).
for size in 16 24 32 48 64 128 256 512; do
  dir="$STAGE/usr/share/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$dir"
  python3 -I - "$ROOT/assets/icon.png" "$dir/vn.chome.Browser.png" "$size" <<'PY'
import sys
from PIL import Image
src, out, size = sys.argv[1], sys.argv[2], int(sys.argv[3])
Image.open(src).resize((size, size), Image.LANCZOS).save(out, optimize=True)
PY
done

install -Dm644 /dev/stdin "$STAGE/usr/share/applications/vn.chome.Browser.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Chỏme
GenericName=Trình duyệt web
Keywords=browser;web;internet;trình duyệt;chome;chỏme;
Comment=Trình duyệt nhẹ, tiết kiệm RAM
Exec=chome %U
Icon=vn.chome.Browser
Terminal=false
Categories=Network;WebBrowser;
MimeType=text/html;application/xhtml+xml;x-scheme-handler/http;x-scheme-handler/https;
StartupWMClass=vn.chome.Browser
StartupNotify=true
Actions=new-window;

[Desktop Action new-window]
Name=Cửa sổ mới
Exec=chome
EOF

install -Dm644 /dev/stdin "$STAGE/usr/share/doc/$PKG/copyright" <<EOF
Chỏme — trình duyệt nhẹ, tiết kiệm RAM
Copyright: $(date +%Y) ${MAINTAINER}
EOF

SIZE_KB="$(du -sk "$STAGE/usr" | cut -f1)"
mkdir -p "$STAGE/DEBIAN"
cat > "$STAGE/DEBIAN/control" <<EOF
Package: $PKG
Version: $VERSION
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: $SIZE_KB
Depends: libc6 (>= 2.34), libwebkitgtk-6.0-4 (>= 2.42), libjavascriptcoregtk-6.0-1 (>= 2.42), libgtk-4-1 (>= 4.12), libsoup-3.0-0, libglib2.0-0t64 | libglib2.0-0, libcairo2, libpango-1.0-0
Recommends: gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-libav, va-driver-all, hunspell-vi, hunspell-en-us
Section: web
Priority: optional
Description: Chỏme — trình duyệt nhẹ, tiết kiệm RAM
 Chỏme là trình duyệt giao diện kiểu Chrome dùng engine WebKit.
 Tab nền tự ngủ để giải phóng RAM, chặn quảng cáo sẵn, lưu lịch sử
 và bookmark cục bộ.
EOF

# Khi cài: thêm kho cập nhật của Chỏme (giống Google Chrome) — chỉ một lần; nếu người dùng
# tự xóa nguồn này thì các lần nâng cấp sau không thêm lại.
cat > "$STAGE/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
SRC=/etc/apt/sources.list.d/chome.sources
MARK=/var/lib/chome/apt-source-added
if [ "$1" = "configure" ] && [ ! -e "$MARK" ]; then
  if [ ! -e "$SRC" ]; then
    cat > "$SRC" <<'SOURCES'
Types: deb
URIs: https://lillinh.github.io/chome/apt
Suites: stable
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/chome-archive-keyring.gpg
SOURCES
  fi
  mkdir -p /var/lib/chome
  touch "$MARK"
fi
exit 0
POSTINST
# Khi gỡ hẳn (apt purge): bỏ nguồn kho và dấu đánh dấu.
cat > "$STAGE/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set -e
if [ "$1" = "purge" ]; then
  rm -f /etc/apt/sources.list.d/chome.sources
  rm -rf /var/lib/chome
fi
exit 0
POSTRM
chmod 755 "$STAGE/DEBIAN/postinst" "$STAGE/DEBIAN/postrm"

mkdir -p dist
OUT="dist/${PKG}_${VERSION}_${ARCH}.deb"
fakeroot dpkg-deb --build --root-owner-group -Zxz "$STAGE" "$OUT" >/dev/null
echo "Đã tạo: $OUT ($(du -h "$OUT" | cut -f1))"
