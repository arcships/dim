#!/bin/sh
set -eu

fail() { printf '%s\n' "$*" >&2; exit 1; }

install_nightly() (
    umask 077
    case $(uname -s) in Darwin) os=darwin;; Linux) os=linux;; *) fail 'Nightly supports macOS and GNU/Linux';; esac
    case $(uname -m) in arm64|aarch64) arch=arm64;; x86_64) arch=x64;; *) fail 'Unsupported nightly CPU';; esac
    command -v jq >/dev/null || fail 'Install jq to read GitHub release metadata'
    root=${XDG_DATA_HOME:-"$HOME/.local/share"}/dim-nightly
    bin=$HOME/.local/bin
    mkdir -p "$bin" "$root"
    lock=$root/install.lock
    mkdir "$lock" || fail "Another installation holds $lock"
    linkdir=
    trap 'rm -rf "$lock"; if [ -n "$linkdir" ]; then rm -rf "$linkdir"; fi' EXIT
    trap 'exit 1' HUP INT TERM
    linkdir=$(mktemp -d "$bin/.dim-nightly.XXXXXX")
    entry=$bin/dim-nightly
    if [ -L "$entry" ]; then
        case $(readlink "$entry") in "$root"/*/bin/dimcode) :;; *) fail "Not a managed nightly: $entry";; esac
    elif [ -e "$entry" ]; then fail "Refusing to replace $entry"; fi

    repo=arcships/dim
    base=https://github.com/$repo/releases/download/Nightly
    for attempt in 1 2; do
        curl -fsSL "https://api.github.com/repos/$repo/releases/tags/Nightly" -o "$lock/release.json"
        version=$(jq -er 'select(.tag_name == "Nightly" and .prerelease and (.draft | not)) | .name | capture("^Nightly v(?<v>[0-9]+\\.[0-9]+\\.[0-9]+-nightly\\.[0-9]+)$").v' "$lock/release.json")
        dest=$root/$version
        [ ! -d "$dest" ] || break
        archive=dimcode-$os-$arch-$version.tar.gz
        if status=$(curl -fsSL "$base/SHA256SUMS-$version" -o "$lock/sums" -w '%{http_code}') &&
           status=$(curl -fsSL "$base/$archive" -o "$lock/$archive" -w '%{http_code}'); then
            break
        fi
        [ "$status" = 404 ] && [ "$attempt" = 1 ] || fail "Could not download Nightly assets for $version (HTTP $status)"
    done
    if [ ! -d "$dest" ]; then
        expected=$(awk -v file="$archive" '$2 == file { print $1 }' "$lock/sums")
        if [ "$os" = darwin ]; then actual=$(shasum -a 256 "$lock/$archive"); else actual=$(sha256sum "$lock/$archive"); fi
        [ "$expected" = "${actual%% *}" ] || fail "Checksum mismatch for $archive"
        mkdir "$lock/package"
        tar -xzf "$lock/$archive" -C "$lock/package"
        [ "$("$lock/package/bin/dimcode" --version)" = "dimcode $version" ] || fail 'Nightly binary version mismatch'
        mv "$lock/package" "$dest"
    fi
    ln -s "$dest/bin/dimcode" "$linkdir/dim-nightly"
    mv -f "$linkdir/dim-nightly" "$entry"
    printf 'Installed dim-nightly %s at %s\n' "$version" "$entry"
    short=$bin/dimn
    if [ ! -e "$short" ] && [ ! -L "$short" ]; then
        { ln -s dim-nightly "$linkdir/dimn" && ln -P "$linkdir/dimn" "$bin/"; } || :
    fi
    if [ -L "$short" ] && [ "$(readlink "$short")" = dim-nightly ]; then
        printf 'Run dimn or dim-nightly to start Nightly.\n'
    elif [ -e "$short" ] || [ -L "$short" ]; then
        printf 'Keeping existing %s; use dim-nightly to start Nightly.\n' "$short" >&2
    else
        printf 'Shortcut unavailable; use dim-nightly to start Nightly.\n' >&2
    fi
    case :$PATH: in *:"$bin":*) :;; *) printf 'Add %s to PATH to run dim-nightly.\n' "$bin";; esac
)

install_nightly
