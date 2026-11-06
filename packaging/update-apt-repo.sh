#!/usr/bin/env bash
# update-apt-repo.sh — thêm gói .deb vào kho APT của Chỏme (phục vụ qua GitHub Pages) và ký lại.
#
# Dùng:  APT_SIGNING_KEY="<khóa bí mật GPG dạng ASCII>" packaging/update-apt-repo.sh <thư mục gh-pages> <file.deb>...
# Kết quả: <thư mục>/apt/{pool,dists/stable/...} + InRelease/Release.gpg đã ký.
# Máy người dùng đọc kho tại https://lillinh.github.io/chome/apt (gói .deb tự thêm nguồn này).
set -euo pipefail

PAGES_DIR="${1:?Thiếu thư mục gh-pages}"
shift
[ "$#" -gt 0 ] || { echo "Thiếu file .deb" >&2; exit 1; }
: "${APT_SIGNING_KEY:?Thiếu biến APT_SIGNING_KEY (khóa bí mật để ký kho)}"
KEEP=3 # số bản giữ lại trong kho

GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
trap 'rm -rf "$GNUPGHOME"' EXIT
printf '%s\n' "$APT_SIGNING_KEY" | gpg --batch --quiet --import
KEY="$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')"

APT_DIR="$PAGES_DIR/apt"
POOL="$APT_DIR/pool/main/c/chome"
mkdir -p "$POOL"
for deb in "$@"; do
  cp "$deb" "$POOL/"
done
# Chỉ giữ KEEP bản mới nhất để kho không phình to.
ls -1 "$POOL"/chome_*_*.deb | sort -V | head -n "-$KEEP" | xargs -r rm -f

cd "$APT_DIR"
for arch in amd64; do
  dir="dists/stable/main/binary-$arch"
  mkdir -p "$dir"
  apt-ftparchive --arch "$arch" packages pool > "$dir/Packages"
  gzip -9fk "$dir/Packages"
done

cd dists/stable
apt-ftparchive \
  -o APT::FTPArchive::Release::Origin=Chome \
  -o APT::FTPArchive::Release::Label=Chome \
  -o APT::FTPArchive::Release::Suite=stable \
  -o APT::FTPArchive::Release::Codename=stable \
  -o APT::FTPArchive::Release::Architectures=amd64 \
  -o APT::FTPArchive::Release::Components=main \
  -o "APT::FTPArchive::Release::Description=Kho cập nhật của trình duyệt Chỏme" \
  release . > "$GNUPGHOME/Release"
mv "$GNUPGHOME/Release" Release
gpg --batch --yes --local-user "$KEY" --clearsign -o InRelease Release
gpg --batch --yes --local-user "$KEY" --armor --detach-sign -o Release.gpg Release
cd ../..

gpg --export "$KEY" > chome-archive-keyring.gpg
touch "$PAGES_DIR/.nojekyll" # GitHub Pages phục vụ file nguyên trạng

cat > "$PAGES_DIR/index.html" <<'EOF'
<!doctype html><meta charset="utf-8"><title>Kho APT của Chỏme</title>
<style>body{font:16px/1.6 system-ui,sans-serif;max-width:640px;margin:10vh auto;padding:0 16px}code{background:#eee;padding:2px 6px;border-radius:4px}</style>
<h1>Kho APT của Chỏme</h1>
<p>Kho này giúp Chỏme tự cập nhật cùng hệ thống. Cài Chỏme từ
<a href="https://github.com/LILLINH/chome/releases/latest">bản phát hành mới nhất</a>
là kho được thêm tự động — không cần làm gì thêm.</p>
<p>Mã nguồn: <a href="https://github.com/LILLINH/chome">github.com/LILLINH/chome</a></p>
EOF

echo "Kho APT đã cập nhật: $(ls "$POOL" | tr '\n' ' ')"
