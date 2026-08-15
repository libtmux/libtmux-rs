#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
workspace_root="$(cd "$script_dir/.." && pwd -P)"
core_root="$workspace_root/crates/libtmux"
macros_root="$workspace_root/crates/libtmux-macros"
mcp_root="$workspace_root/crates/tmux-mcp"
workspace_crate_root="$workspace_root/crates/tmux-workspace"

cd "$workspace_root"

core_package="$(cargo package --locked --allow-dirty --package libtmux --list)"
macros_package="$(cargo package --locked --allow-dirty --package libtmux-macros --list)"
mcp_package="$(cargo package --locked --allow-dirty --package tmux-mcp --list)"
workspace_crate_package="$(cargo package --locked --allow-dirty --package tmux-workspace --list)"

contains_line() {
    local lines="$1"
    local expected="$2"
    local line

    while IFS= read -r line; do
        if [[ "$line" == "$expected" ]]; then
            return 0
        fi
    done <<< "$lines"

    return 1
}

require_line() {
    local lines="$1"
    local expected="$2"
    local package="$3"

    if ! contains_line "$lines" "$expected"; then
        printf '%s package is missing %s\n' "$package" "$expected" >&2
        return 1
    fi
}

shopt -s nullglob

# The crate's front page links docs/design.md and names the examples, so both
# ship or those references break for anyone reading the packaged crate.
core_files=(
    "$core_root"/LICENSE-APACHE
    "$core_root"/LICENSE-MIT
    "$core_root"/README.md
    "$core_root"/docs/design.md
    "$core_root"/docs/parity.md
    "$core_root"/examples/*.rs
    "$core_root"/schema/*.json
    "$core_root"/tests/fixtures/filter-v1/*.json
    "$core_root"/tests/fixtures/dialect-v1/*.json
)
macros_files=(
    "$macros_root"/LICENSE-APACHE
    "$macros_root"/LICENSE-MIT
    "$macros_root"/README.md
    "$macros_root"/tests/ui/pass/*.rs
    "$macros_root"/tests/ui/fail/*.rs
    "$macros_root"/tests/ui/fail/*.stderr
)
# The MCP front page names its examples, so those ship or the references
# break. Its planning notes under docs/ deliberately do not: they are working
# material for this repository, not something an integrator needs in a tarball.
mcp_files=(
    "$mcp_root"/LICENSE-APACHE
    "$mcp_root"/LICENSE-MIT
    "$mcp_root"/README.md
    "$mcp_root"/examples/*.rs
)
# `docs/libtmux-macros-README.md` is a symlink to the copy that crate owns,
# and the doctest verifying it reads that path. It has to ship, or
# `cargo test --doc` fails for anyone using the published crate.
workspace_crate_files=(
    "$workspace_crate_root"/LICENSE-APACHE
    "$workspace_crate_root"/LICENSE-MIT
    "$workspace_crate_root"/README.md
    "$workspace_crate_root"/libtmux-macros-README.md
)

# nullglob is on, so an empty list would make every check below vacuous.
# These files are the reason this script exists; none of them is optional.
if (( ${#core_files[@]} == 0 || ${#macros_files[@]} == 0 || ${#mcp_files[@]} == 0 \
      || ${#workspace_crate_files[@]} == 0 )); then
    printf 'expected packaged files are missing from the working tree\n' >&2
    exit 1
fi

for file in "${core_files[@]}"; do
    require_line "$core_package" "${file#"$core_root"/}" "libtmux"
done

for file in "${macros_files[@]}"; do
    require_line "$macros_package" "${file#"$macros_root"/}" "libtmux-macros"
done

for file in "${mcp_files[@]}"; do
    require_line "$mcp_package" "${file#"$mcp_root"/}" "tmux-mcp"
done

for file in "${workspace_crate_files[@]}"; do
    require_line "$workspace_crate_package" \
        "${file#"$workspace_crate_root"/}" "tmux-workspace"
done

# A shipped document that tells the reader which version to depend on has to
# name this one. The copy on crates.io is frozen at publish time, so a stale
# number there sends every reader of the new release to the old one, and no
# amount of fixing it afterwards reaches them without another release.
check_documented_version() {
    local root="$1" name="$2" doc line found_crate found status=0

    while IFS= read -r doc; do
        [[ -f "$root/$doc" ]] || continue
        case "$doc" in
            *.md) ;;
            *) continue ;;
        esac
        # Each reference is checked against the version of the crate it names,
        # not against one crate's version. tmux-mcp carries its own, so a
        # document that mentions both cannot be right about both otherwise.
        while IFS= read -r line; do
            found_crate="${line%% *}"
            found="${line#* }"
            if [[ "$found" != "${crate_versions[$found_crate]}" ]]; then
                printf '%s ships %s, which tells the reader to depend on %s %s, not %s\n' \
                    "$name" "$doc" "$found_crate" "$found" \
                    "${crate_versions[$found_crate]}" >&2
                status=1
            fi
        done < <(grep -oP '\b(?:libtmux|libtmux-macros|tmux-mcp)\b(?=\s*=)\s*=\s*(?:\{[^}]*version\s*=\s*)?"[^"]+"' \
                     "$root/$doc" \
                     | sed -E 's/^([a-z-]+).*"([^"]+)"$/\1 \2/' || true)
    done <<< "$3"

    return "$status"
}

declare -A crate_versions=()
metadata="$(cargo metadata --format-version 1 --no-deps)"
for crate in libtmux libtmux-macros tmux-mcp tmux-workspace; do
    crate_versions[$crate]="$(
        printf '%s' "$metadata" \
            | grep -oP "\"name\":\"$crate\",\"version\":\"\K[^\"]+"
    )"
    if [[ -z "${crate_versions[$crate]}" ]]; then
        printf 'could not read the version of %s\n' "$crate" >&2
        exit 1
    fi
done

check_documented_version "$core_root" libtmux "$core_package"
check_documented_version "$macros_root" libtmux-macros "$macros_package"
check_documented_version "$mcp_root" tmux-mcp "$mcp_package"
check_documented_version "$workspace_crate_root" tmux-workspace "$workspace_crate_package"

# A relative link in a shipped document has to point at something else that
# ships. A file that exists in the working tree but is left out of `include`
# reads fine here and 404s in the tarball and on docs.rs, which is the one
# place nobody checks.
check_relative_links() {
    local package="$1" root="$2" name="$3" doc target resolved status=0

    while IFS= read -r doc; do
        case "$doc" in
            *.md) ;;
            *) continue ;;
        esac

        while IFS= read -r target; do
            # Skip URLs and same-page anchors; neither resolves to a file.
            case "$target" in
                http://* | https://* | '#'*) continue ;;
            esac
            target="${target%%#*}"
            [[ -n "$target" ]] || continue

            # `-s` so a link is judged by the path it takes in the tarball
            # rather than by where a symlink points in the working tree. The
            # per-crate LICENSE files are symlinks to the pair at the
            # repository root, and `cargo package` writes them out as real
            # files, so the sibling path is the one that has to resolve.
            resolved="$(cd "$root/$(dirname "$doc")" \
                && realpath -m -s --relative-to="$root" "$target")"
            if ! contains_line "$package" "$resolved"; then
                printf '%s ships %s, which links %s -> %s, which it does not ship\n' \
                    "$name" "$doc" "$target" "$resolved" >&2
                status=1
            fi
        done < <(grep -oP '\]\(\K[^)]+' "$root/$doc" || true)
    done <<< "$package"

    return "$status"
}

check_relative_links "$core_package" "$core_root" libtmux
check_relative_links "$macros_package" "$macros_root" libtmux-macros
check_relative_links "$mcp_package" "$mcp_root" tmux-mcp
check_relative_links "$workspace_crate_package" "$workspace_crate_root" tmux-workspace

while IFS= read -r entry; do
    case "$entry" in
        libtmux-macros/*)
            printf 'libtmux package contains macro source: %s\n' "$entry" >&2
            exit 1
            ;;
    esac
done <<< "$core_package"

macros_edges="$(cargo tree --locked --package libtmux-macros --edges normal,build --prefix none)"

while IFS= read -r entry; do
    case "$entry" in
        "libtmux v"*)
            printf 'libtmux-macros has a normal or build edge to libtmux: %s\n' "$entry" >&2
            exit 1
            ;;
    esac
done <<< "$macros_edges"

core_edges="$(cargo tree --locked --package libtmux --no-default-features --edges normal --prefix none)"

while IFS= read -r entry; do
    case "$entry" in
        "serde v"* | "serde_core v"* | "serde_derive v"* | "libtmux-macros v"*)
            printf 'libtmux default-free normal edges contain an optional dependency: %s\n' "$entry" >&2
            exit 1
            ;;
    esac
done <<< "$core_edges"
