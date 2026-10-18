#!/bin/sh
# Download a prebuilt Spacewalk release. No Rust toolchain or root access needed.
set -eu

main() {
    version=${SPACEWALK_VERSION:-latest}
    install_dir=${SPACEWALK_INSTALL_DIR:-"${HOME:?HOME is not set}/.local/bin"}
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version|--install-dir)
                [ "$#" -ge 2 ] || fail "$1 needs a value"
                case "$1" in
                    --version) version=$2 ;;
                    --install-dir) install_dir=$2 ;;
                esac
                shift 2
                ;;
            -h|--help)
                printf '%s\n' 'Usage: sh install.sh [--version v0.1.0] [--install-dir /path/to/bin]' \
                    'Defaults: latest release, ~/.local/bin. Re-run to update.'
                return
                ;;
            *) fail "Unknown option: $1 (see --help)" ;;
        esac
    done

    case "$version" in
        latest) release_path=latest/download ;;
        v[0-9]*)
            case "$version" in *[!a-zA-Z0-9._-]*) fail "Invalid version: $version" ;; esac
            release_path=download/$version
            ;;
        *) fail 'Version must be latest or a tag such as v0.1.0' ;;
    esac
    [ -n "$install_dir" ] || fail 'Installation directory cannot be empty'
    # Make paths absolute so filenames starting with a dash are safe, too.
    case "$install_dir" in /*) ;; *) install_dir=$PWD/$install_dir ;; esac

    os=$(uname -s)
    arch=$(uname -m)
    case "$os/$arch" in
        Linux/x86_64|Linux/amd64) target=x86_64-unknown-linux-musl ;;
        Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-musl ;;
        Darwin/x86_64) target=x86_64-apple-darwin ;;
        Darwin/arm64|Darwin/aarch64) target=aarch64-apple-darwin ;;
        *) fail "No prebuilt binary for $os/$arch. See the README for installation with Cargo." ;;
    esac
    if command -v curl >/dev/null 2>&1; then
        downloader=curl
    elif command -v wget >/dev/null 2>&1; then
        downloader=wget
    else
        fail 'Install curl or wget first'
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        checksum=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        checksum=shasum
    else
        fail 'Install sha256sum or shasum first'
    fi
    for tool in tar mktemp mkdir chmod mv rm; do
        command -v "$tool" >/dev/null 2>&1 || fail "Required command not found: $tool"
    done

    tmp_dir=$(mktemp -d)
    staged_binary=
    trap 'rm -rf "$tmp_dir"; if [ -n "$staged_binary" ]; then rm -f "$staged_binary"; fi' 0
    trap 'exit 1' HUP INT TERM
    asset=spacewalk-$target.tar.gz
    base_url=https://github.com/mattslaney/spacewalk/releases/$release_path
    printf 'Downloading Spacewalk (%s, %s)...\n' "$version" "$target"
    download "$base_url/$asset" "$tmp_dir/$asset"
    download "$base_url/$asset.sha256" "$tmp_dir/checksum"

    read -r expected checksum_name extra < "$tmp_dir/checksum" || fail 'Invalid checksum file'
    [ "${#expected}" -eq 64 ] || fail 'Invalid checksum file'
    case "$expected" in *[!a-fA-F0-9]*) fail 'Invalid checksum file' ;; esac
    [ "$checksum_name" = "$asset" ] && [ -z "$extra" ] || fail 'Unexpected checksum filename'
    if [ "$checksum" = sha256sum ]; then
        actual=$(sha256sum "$tmp_dir/$asset")
    else
        actual=$(shasum -a 256 "$tmp_dir/$asset")
    fi
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || fail 'Checksum verification failed; nothing was installed'

    mkdir -p "$install_dir"
    [ ! -d "$install_dir/spacewalk" ] || fail "$install_dir/spacewalk is a directory"
    staged_binary=$(mktemp "$install_dir/.spacewalk.XXXXXXXX")
    # Extract only the executable to a temporary file, never archive paths.
    tar -xzOf "$tmp_dir/$asset" spacewalk > "$staged_binary" || fail 'Cannot extract release binary'
    [ -s "$staged_binary" ] || fail 'Release binary is empty'
    chmod 755 "$staged_binary"
    mv -f "$staged_binary" "$install_dir/spacewalk"
    staged_binary=
    printf 'Installed Spacewalk to %s/spacewalk\n' "$install_dir"
    case ":${PATH:-}:" in
        *:"$install_dir":*) printf '%s\n' 'Run: spacewalk --help' ;;
        *)
            printf 'Add %s to your PATH, then run spacewalk --help.\n' "$install_dir"
            printf '%s\n' 'For the default directory: export PATH="$HOME/.local/bin:$PATH"'
            ;;
    esac
}

fail() {
    printf 'spacewalk installer: %s\n' "$*" >&2
    exit 1
}

download() {
    if [ "$downloader" = curl ]; then
        curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
            --connect-timeout 15 --max-time 300 --retry 3 --output "$2" "$1" ||
            fail "Could not download $1. Check connectivity and that the release is published."
    else
        wget --quiet --timeout=30 --tries=3 --output-document="$2" "$1" ||
            fail "Could not download $1. Check connectivity and that the release is published."
    fi
}

main "$@"
