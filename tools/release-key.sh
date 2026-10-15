#!/bin/bash
# Makes Game Bridge's release signing key, once. The password is random, goes
# straight into KDE Wallet (wallet kdewallet, folder GameBridge, entry
# release-key) and is never printed; the key itself goes to
# ~/dev/game-bridge-key/gamebridge-release.p12, outside the public repo.
# tools/build-app.sh --public reads both. Losing the key means people must
# uninstall Game Bridge to update it, so the key folder and the wallet are in
# the daily Kopia backup.
#
# usage: tools/release-key.sh
set -euo pipefail
KEY_DIR=$HOME/dev/game-bridge-key
KEYSTORE=$KEY_DIR/gamebridge-release.p12
WALLET=kdewallet FOLDER=GameBridge ENTRY=release-key

if [ -e "$KEYSTORE" ]; then
    echo "The key already exists: $KEYSTORE (not replaced)" >&2
    exit 1
fi

# kwallet-query can't make folders, so ask the wallet service directly.
call() { qdbus6 org.kde.kwalletd6 /modules/kwalletd6 "$@"; }
handle=$(call org.kde.KWallet.open "$WALLET" 0 game-bridge-release-key)
[ "$(call org.kde.KWallet.hasFolder "$handle" "$FOLDER" game-bridge-release-key)" = true ] ||
    call org.kde.KWallet.createFolder "$handle" "$FOLDER" game-bridge-release-key >/dev/null

if kwallet-query -f "$FOLDER" -r "$ENTRY" "$WALLET" >/dev/null 2>&1 &&
    [ -n "$(kwallet-query -f "$FOLDER" -r "$ENTRY" "$WALLET" 2>/dev/null)" ]; then
    echo "KDE Wallet already holds a release-key password; not replacing it." >&2
    exit 1
fi
password=$(head -c 30 /dev/urandom | base64 | tr -d '/+=')
printf %s "$password" | kwallet-query -f "$FOLDER" -w "$ENTRY" "$WALLET"
# Read back: only build with a password the wallet really holds.
[ "$(kwallet-query -f "$FOLDER" -r "$ENTRY" "$WALLET")" = "$password" ] || { echo "KDE Wallet didn't keep the password." >&2; exit 1; }

mkdir -p "$KEY_DIR"
chmod 700 "$KEY_DIR"
export GAMEBRIDGE_KEY_PASSWORD=$password
keytool -genkeypair -keystore "$KEYSTORE" -storetype PKCS12 -alias gamebridge \
    -keyalg RSA -keysize 4096 -validity 10000 \
    -dname "CN=Braxten Chenay, O=Game Bridge" \
    -storepass:env GAMEBRIDGE_KEY_PASSWORD -keypass:env GAMEBRIDGE_KEY_PASSWORD
chmod 600 "$KEYSTORE"
keytool -list -keystore "$KEYSTORE" -storepass:env GAMEBRIDGE_KEY_PASSWORD | grep -i -A1 "gamebridge"
echo "made $KEYSTORE"
