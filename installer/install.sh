#!/bin/sh
# Per-user installation. Enrollment runs in the foreground; no hidden daemon.
set -eu
controller= token= name= base= pin=
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || { echo 'Missing argument value' >&2; exit 2; }
    case "$1" in
        --controller) controller=$2 ;;
        --enroll-token) token=$2 ;;
        --name) name=$2 ;;
        --download-base) base=$2 ;;
        --tls-pin) pin=$2 ;;
        *) echo 'Unknown installer argument' >&2; exit 2 ;;
    esac
    shift 2
done
[ -n "$controller" ] && [ -n "$token" ] && [ -n "$base" ] || {
    echo 'Required: --controller, --enroll-token, --download-base' >&2; exit 2;
}
case "$base" in https://*) ;; *) echo 'Downloads require HTTPS' >&2; exit 2 ;; esac
case "$(uname -s)" in Linux) os=linux ;; Darwin) os=macos ;; *) echo 'Unsupported OS' >&2; exit 2 ;; esac
case "$(uname -m)" in x86_64|amd64) arch=x86_64 ;; arm64|aarch64) arch=aarch64 ;; *) echo 'Unsupported architecture' >&2; exit 2 ;; esac
asset=rustfrp-agent-$os-$arch
dir=${HOME:?}/.rustfrp
umask 077
mkdir -p "$dir/bin"
work=$(mktemp -d "$dir/install.XXXXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 10 --max-time 300 "${base%/}/$asset" -o "$work/agent"
curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 10 --max-time 30 "${base%/}/$asset.sha256" -o "$work/hash"
expected=$(cat "$work/hash")
[ "${#expected}" -eq 64 ] || { echo 'Invalid SHA256 manifest' >&2; exit 1; }
case "$expected" in *[!0-9a-fA-F]*) echo 'Invalid SHA256 manifest' >&2; exit 1 ;; esac
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work/agent")
else
    actual=$(shasum -a 256 "$work/agent")
fi
actual=${actual%% *}
[ "$actual" = "$expected" ] || { echo 'SHA256 mismatch; existing agent unchanged' >&2; exit 1; }
chmod 700 "$work/agent"
mv "$work/agent" "$dir/bin/rustfrp-agent"
rm -rf "$work"
trap - EXIT HUP INT TERM
# Identity persists here; the one-time token is never written to a service file.
set --
if [ -n "$pin" ]; then set -- --tls-pin "$pin"; fi
exec "$dir/bin/rustfrp-agent" --mode client --controller "$controller" --enroll-token "$token" --name "$name" --state-dir "$dir" "$@"
