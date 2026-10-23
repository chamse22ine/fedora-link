#!/usr/bin/env bash
# Construit le .rpm localement : archive des sources + dépendances Rust embarquées.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^Version:[[:space:]]*//p' "$root/packaging/fedora-link.spec")"
name="fedora-link-$version"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/$name"
tar -C "$root" --exclude=./target --exclude=./.git --exclude=./vendor -cf - . | tar -C "$work/$name" -xf -

# Sources des dépendances dans l'archive : le build RPM se fait hors ligne.
(
    cd "$work/$name"
    mkdir -p .cargo
    cargo vendor --locked vendor > .cargo/config.toml
)

mkdir -p "$HOME/rpmbuild/SOURCES" "$HOME/rpmbuild/SPECS"
tar -C "$work" -czf "$HOME/rpmbuild/SOURCES/$name.tar.gz" "$name"
cp "$root/packaging/fedora-link.spec" "$HOME/rpmbuild/SPECS/"
# RPMBUILD_ARGS=--nodeps pour un build local avec une toolchain rustup.
# shellcheck disable=SC2086
rpmbuild ${RPMBUILD_ARGS:-} -ba "$HOME/rpmbuild/SPECS/fedora-link.spec"
