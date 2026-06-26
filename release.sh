#!/usr/bin/env bash
#
# release.sh — собирает release-бинарь cozby-claw и «релизит» его в общий
# каталог на PATH (по умолчанию ~/.local/bin), чтобы его можно было звать
# из любого места как `cozby-claw-cli`.
#
# Каталог установки переопределяется переменной COZBY_BIN_DIR:
#     COZBY_BIN_DIR=/usr/local/bin ./release.sh
#
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUST_DIR="$REPO_DIR/rust"
INSTALL_DIR="${COZBY_BIN_DIR:-$HOME/.local/bin}"
BINS=(cozby-claw-cli)

echo "==> cargo build --release (cozby-claw-cli)"
( cd "$RUST_DIR" && cargo build --release -p rusty-claude-cli )

echo "==> installing into $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
for bin in "${BINS[@]}"; do
    src="$RUST_DIR/target/release/$bin"
    [[ -x "$src" ]] || { echo "!! missing built binary: $src" >&2; exit 1; }
    # install копирует с правами 0755 и атомарно заменяет старую версию.
    install -m 0755 "$src" "$INSTALL_DIR/$bin"
    echo "   $bin -> $INSTALL_DIR/$bin"
done

echo "==> ensuring $INSTALL_DIR is on PATH"
if command -v fish >/dev/null 2>&1; then
    # fish_add_path персистентен (universal fish_user_paths) и идемпотентен.
    fish -c "fish_add_path '$INSTALL_DIR'"
    echo "   fish: fish_add_path $INSTALL_DIR"
else
    # POSIX-оболочки (bash/zsh): дописываем в ~/.profile один раз.
    profile="$HOME/.profile"
    if ! grep -qsF "$INSTALL_DIR" "$profile" 2>/dev/null; then
        printf '\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$profile"
        echo "   appended PATH export to $profile"
    else
        echo "   $profile already references $INSTALL_DIR"
    fi
fi

echo
echo "==> done. Open a new shell, then run:"
for bin in "${BINS[@]}"; do echo "      $bin"; done
