#!/usr/bin/env bash
# release.sh — phát hành bản mới của Chỏme bằng một lệnh.
#
# Dùng:  packaging/release.sh 0.2.0
# Việc làm: đổi số phiên bản trong Cargo.toml → commit → tạo tag v0.2.0 → đẩy lên GitHub.
# Sau đó GitHub Actions (.github/workflows/release.yml) tự build .deb và tạo bản phát hành;
# người dùng đã cài Chỏme sẽ được báo có bản mới.
set -euo pipefail

VERSION="${1:-}"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Cách dùng: packaging/release.sh <phiên bản dạng X.Y.Z, ví dụ 0.2.0>" >&2
  exit 1
fi

cd "$(dirname "$0")/.."

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Còn thay đổi chưa commit — hãy commit trước khi phát hành." >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  echo "Tag v$VERSION đã tồn tại." >&2
  exit 1
fi

CURRENT="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
echo "Phiên bản: $CURRENT → $VERSION"

# Chỉ đổi dòng version đầu tiên (của gói chome), không đụng phiên bản thư viện.
sed -i "0,/^version = \".*\"/s//version = \"$VERSION\"/" Cargo.toml
# Cập nhật Cargo.lock cho khớp (không cần mạng).
cargo metadata --format-version 1 --offline >/dev/null 2>&1 || cargo metadata --format-version 1 >/dev/null

git add Cargo.toml Cargo.lock
git commit -m "Phát hành $VERSION"
git tag -a "v$VERSION" -m "Chỏme $VERSION"
git push origin HEAD "v$VERSION"

echo
echo "Đã đẩy tag v$VERSION."
echo "Theo dõi quá trình build: https://github.com/LILLINH/chome/actions"
