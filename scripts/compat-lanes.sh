# Which tmux lanes the compatibility script builds and tests. Sourced by
# test-tmux-format-compat.sh and by its tests, never run.
#
# .github/ci-matrix.txt lists the lanes, each with the commit its tag must name.
# LIBTMUX_COMPAT_LANES selects some of them, and may name a tmux version, tag,
# branch or commit the manifest does not hold. The caller defines
# `build_lane TAG` and `test_lane TAG`; a lane's prerequisites are built, and
# not tested, before it.

# shellcheck disable=SC2034 # the sourcing script reads these tables
lane_tags=()
declare -gA lane_kind=() lane_commit=() lane_version=() lane_contract=() lane_requires=()

compat_repository=${LIBTMUX_COMPAT_TMUX_REPOSITORY:-https://github.com/tmux/tmux.git}

# A tag, branch or commit of tmux. A name that starts with a dash would read as
# a git option.
valid_lane_ref() {
    [[ $1 =~ ^[A-Za-z0-9._/-]{1,64}$ && $1 != -* ]]
}

# load_lanes MANIFEST reads the lanes in file order.
load_lanes() {
    local manifest=$1
    local kind tag commit version contract requires extra
    local required
    local -a needs

    lane_tags=()
    lane_kind=()
    lane_commit=()
    lane_version=()
    lane_contract=()
    lane_requires=()
    while read -r kind tag commit version contract requires extra; do
        [[ -n "$kind" ]] || continue
        if [[ -z "$requires" || -n "$extra" ]]; then
            printf 'malformed lane in %s: %s\n' "$manifest" "$kind $tag" >&2
            return 1
        fi
        case "$kind" in
            supported | witness | refused | development) ;;
            *)
                printf 'unknown lane kind in %s: %s\n' "$manifest" "$kind" >&2
                return 1
                ;;
        esac
        if ! valid_lane_ref "$tag"; then
            printf 'invalid lane tag in %s: %s\n' "$manifest" "$tag" >&2
            return 1
        fi
        if [[ ! "$commit" =~ ^[0-9a-f]{40}$ ]]; then
            printf 'lane %s does not name a 40-hex commit\n' "$tag" >&2
            return 1
        fi
        if [[ -n "${lane_kind[$tag]:-}" ]]; then
            printf 'lane %s is listed twice\n' "$tag" >&2
            return 1
        fi
        lane_tags+=("$tag")
        lane_kind[$tag]="$kind"
        lane_commit[$tag]="$commit"
        lane_version[$tag]="$version"
        lane_contract[$tag]="$contract"
        lane_requires[$tag]="$requires"
    done < <(sed 's/#.*//' "$manifest")

    if (( ${#lane_tags[@]} == 0 )); then
        printf '%s lists no lane\n' "$manifest" >&2
        return 1
    fi
    for tag in "${lane_tags[@]}"; do
        [[ "${lane_requires[$tag]}" != - ]] || continue
        IFS=, read -r -a needs <<< "${lane_requires[$tag]}"
        for required in "${needs[@]}"; do
            if [[ -z "${lane_kind[$required]:-}" ]]; then
                printf 'lane %s requires %s, which the manifest does not list\n' "$tag" "$required" >&2
                return 1
            fi
        done
    done
}

# select_lanes prints the lanes to run, one per line: every lane in the
# manifest, or the ones LIBTMUX_COMPAT_LANES names, each once, in the order given.
select_lanes() {
    local request=${LIBTMUX_COMPAT_LANES:-}
    local lane
    local -a requested=()
    local -a chosen=()

    if [[ -z "${request//[[:space:],]/}" ]]; then
        printf '%s\n' "${lane_tags[@]}"
        return 0
    fi
    read -r -a requested <<< "${request//,/ }"
    for lane in "${requested[@]}"; do
        if ! valid_lane_ref "$lane"; then
            printf 'invalid lane in LIBTMUX_COMPAT_LANES: %s\n' "$lane" >&2
            return 1
        fi
        case " ${chosen[*]:-} " in
            *" $lane "*) ;;
            *) chosen+=("$lane") ;;
        esac
    done
    printf '%s\n' "${chosen[@]}"
}

# resolve_lane_ref REF prints the commit a tag, branch or commit of tmux
# names. A tag wins over a branch, and an annotated tag is followed to the
# commit it tags.
resolve_lane_ref() {
    local ref=$1
    local listing
    local commit
    local name

    if [[ "$ref" =~ ^[0-9a-f]{40}$ ]]; then
        printf '%s\n' "$ref"
        return 0
    fi
    if ! listing="$(git ls-remote -- "$compat_repository" "refs/tags/$ref" "refs/tags/$ref^{}" "refs/heads/$ref")"; then
        printf 'could not list %s\n' "$compat_repository" >&2
        return 1
    fi
    for name in "refs/tags/$ref^{}" "refs/tags/$ref" "refs/heads/$ref"; do
        commit="$(awk -v name="$name" '$2 == name { print $1 }' <<< "$listing")"
        if [[ -n "$commit" ]]; then
            printf '%s\n' "$commit"
            return 0
        fi
    done
    printf '%s has no tag or branch named %s\n' "$compat_repository" "$ref" >&2
    return 1
}

# run_selected builds and tests the selected lanes.
run_selected() {
    local selected
    local lane
    local required
    local -a lanes
    local -a needs

    selected="$(select_lanes)" || return 1
    mapfile -t lanes <<< "$selected"
    for lane in "${lanes[@]}"; do
        if [[ -n "${lane_kind[$lane]:-}" && "${lane_requires[$lane]}" != - ]]; then
            IFS=, read -r -a needs <<< "${lane_requires[$lane]}"
            for required in "${needs[@]}"; do
                build_lane "$required"
            done
        fi
        build_lane "$lane"
        test_lane "$lane"
    done
}
