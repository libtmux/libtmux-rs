#!/usr/bin/env bash

set -euo pipefail

readonly internal_worker_mode="__libtmux_tfc_worker"
build_root=""

select_pidfd_python() {
    local candidate
    local duplicate
    local previous
    local -a seen=()

    while IFS= read -r candidate; do
        if [[ -z "$candidate" || ! -x "$candidate" ]]; then
            continue
        fi
        duplicate=0
        for previous in "${seen[@]}"; do
            if [[ "$candidate" == "$previous" ]]; then
                duplicate=1
                break
            fi
        done
        if (( duplicate )); then
            continue
        fi
        seen+=("$candidate")
        if "$candidate" -c \
            'import os, signal; fd = os.pidfd_open(os.getpid()); os.close(fd); raise SystemExit(0 if hasattr(signal, "pidfd_send_signal") else 1)' \
            </dev/null \
            >/dev/null \
            2>&1; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done < <(type -aP python3 2>/dev/null || :)
    return 1
}

if [[ "${1:-}" == "$internal_worker_mode" ]]; then
    if (( $# != 2 )); then
        printf 'invalid compatibility worker invocation\n' >&2
        exit 1
    fi
    build_root="$2"
    case "$build_root" in
        /tmp/libtmux-tmux-format-compat.*) ;;
        *)
            printf 'invalid compatibility worker build root\n' >&2
            exit 1
            ;;
    esac
    if [[ -L "$build_root" || ! -d "$build_root" ]]; then
        printf 'invalid compatibility worker build root\n' >&2
        exit 1
    fi
    kill -STOP -- "$BASHPID"
else
    if (( $# != 0 )); then
        printf 'tmux format compatibility accepts no arguments\n' >&2
        exit 1
    fi

    kernel_name=""
    if ! kernel_name="$(uname -s)"; then
        printf 'tmux format compatibility requires uname\n' >&2
        exit 1
    fi
    if [[ "$kernel_name" != "Linux" ]]; then
        printf 'tmux format compatibility requires Linux\n' >&2
        exit 1
    fi

    locale_charmap=""
    if ! locale_charmap="$(LC_ALL=C.UTF-8 locale charmap)"; then
        printf 'tmux format compatibility requires C.UTF-8 locale\n' >&2
        exit 1
    fi
    if [[ "$locale_charmap" != "UTF-8" ]]; then
        printf 'tmux format compatibility requires C.UTF-8\n' >&2
        exit 1
    fi

    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
    supervisor="$script_dir/tmux-format-compat-supervisor.py"
    python_binary="$(select_pidfd_python || :)"
    bash_binary="$(type -P bash || :)"
    if [[ -z "$python_binary" || ! -x "$python_binary" || ! -f "$supervisor" ]]; then
        printf 'tmux format compatibility requires Python pidfd support\n' >&2
        exit 1
    fi
    if [[ -z "$bash_binary" || ! -x "$bash_binary" ]]; then
        printf 'tmux format compatibility requires Bash\n' >&2
        exit 1
    fi
    exec "$python_binary" \
        "$supervisor" \
        -- \
        "$bash_binary" \
        "$script_dir/${BASH_SOURCE[0]##*/}" \
        "$internal_worker_mode"
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
workspace_root="$(cd "$script_dir/.." && pwd -P)"
cd "$workspace_root"

# The lanes, and the commit each one must build, are listed in the manifest.
# LIBTMUX_COMPAT_LANES selects some of them, or names a tmux the manifest does
# not hold.
# shellcheck source=scripts/compat-lanes.sh
source "$script_dir/compat-lanes.sh"
load_lanes "$workspace_root/.github/ci-matrix.txt"

require_family() {
    local test_list="$1"
    local family="$2"

    case "$test_list" in
        *"$family"*) ;;
        *)
            printf 'missing required Rust test family: %s\n' "$family" >&2
            return 1
            ;;
    esac
}

normalize_bootstrap_mtimes() {
    local checkout_epoch="$1"

    {
        git ls-files -z --cached --others --exclude-standard
        git ls-files -z --others --ignored --exclude-standard
    } | xargs -0r touch --no-dereference --date="@$checkout_epoch" --
}

# The lane's tmux, built from its pinned commit. A lane the manifest does not
# hold is a tag, branch or commit named by LIBTMUX_COMPAT_LANES: it resolves to
# a commit and is built as a development build, with no version to compare.
declare -A lane_built=()

build_lane() {
    local tag="$1"
    local key="${tag//\//_}"
    local expected_commit="${lane_commit[$tag]:-}"
    local expected_version=""
    local source_contract="${lane_contract[$tag]:-custom}"
    local source_root="$build_root/tmux-$key-source"
    local install_prefix="$build_root/tmux-$key-install"
    local tmux_binary="$install_prefix/bin/tmux"
    local tag_type
    local peeled_commit
    local checkout_commit
    local checkout_epoch
    local installed_version

    if [[ -n "${lane_built[$tag]:-}" ]]; then
        return 0
    fi
    if [[ -n "${lane_kind[$tag]:-}" ]]; then
        expected_version="tmux ${lane_version[$tag]}"
    else
        expected_commit="$(resolve_lane_ref "$tag")"
    fi

    if [[ "$source_contract" == "development" || "$source_contract" == "custom" ]]; then
        # A development build has no release tag: pin the exact commit.
        git init "$source_root"
        git -C "$source_root" fetch --depth 1 \
            "$compat_repository" "$expected_commit"
        git -C "$source_root" checkout --detach FETCH_HEAD
    else
        git clone \
            --branch "$tag" \
            --depth 1 \
            --single-branch \
            "$compat_repository" \
            "$source_root"

        tag_type="$(git -C "$source_root" cat-file -t "refs/tags/$tag")"
        if [[ "$tag_type" != "tag" ]]; then
            printf 'tmux %s is not an annotated tag\n' "$tag" >&2
            return 1
        fi

        peeled_commit="$(git -C "$source_root" rev-parse "refs/tags/$tag^{}")"
        if [[ "$peeled_commit" != "$expected_commit" ]]; then
            printf 'tmux %s peeled commit does not match\n' "$tag" >&2
            return 1
        fi
    fi

    checkout_commit="$(git -C "$source_root" rev-parse HEAD)"
    if [[ "$checkout_commit" != "$expected_commit" ]]; then
        printf 'tmux %s checkout commit does not match\n' "$tag" >&2
        return 1
    fi
    checkout_epoch="$(git -C "$source_root" show -s --format=%ct "$checkout_commit")"

    if [[ "$source_contract" == "format-catalog" ]]; then
        python3 scripts/format-coverage.py --check-source "$source_root"
    fi

    (
        cd "$source_root"
        sh autogen.sh
        # Automake rejects a new conftest when the wall clock steps backward.
        # Keep this disposable tree at its authenticated commit time instead.
        normalize_bootstrap_mtimes "$checkout_epoch"
        ./configure --prefix="$install_prefix"
        make -j"$(nproc)"
        make install
    )

    if [[ ! -x "$tmux_binary" ]]; then
        printf 'tmux %s did not install an executable\n' "$tag" >&2
        return 1
    fi

    installed_version="$("$tmux_binary" -V)"
    if [[ -n "$expected_version" && "$installed_version" != "$expected_version" ]]; then
        printf 'tmux %s version output does not match\n' "$tag" >&2
        return 1
    fi
    printf '%s\n' "$installed_version"
    lane_built[$tag]=1
}

test_lane() {
    local tag="$1"
    local key="${tag//\//_}"
    local source_contract="${lane_contract[$tag]:-custom}"
    local expected_version="tmux ${lane_version[$tag]:-}"
    local install_prefix="$build_root/tmux-$key-install"
    local tmux_binary="$install_prefix/bin/tmux"
    local test_list

    # A filtered run that matches no test exits 0, so every refusal lane goes
    # through require-tests.sh. LIBTMUX_REQUIRE_NATIVE names the variables
    # a lane provides: a case gated on one fails instead of skipping.
    if [[ "$source_contract" == "reject-core" ]]; then
        LIBTMUX_TEST_UNSUPPORTED_TMUX="$tmux_binary" \
            LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_UNSUPPORTED_TMUX \
            bash scripts/require-tests.sh \
            cargo test --locked --package libtmux --all-features \
            --test daemon_version_check -- --test-threads=1
        LIBTMUX_TEST_UNSUPPORTED_TMUX="$tmux_binary" \
            LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_UNSUPPORTED_TMUX \
            bash scripts/require-tests.sh \
            cargo test --locked --package tmux-workspace --all-features \
            --test cli old_daemon_refuses_load_before_scripts_or_mutation \
            -- --exact --test-threads=1
        LIBTMUX_TEST_UNSUPPORTED_TMUX="$tmux_binary" \
            LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_UNSUPPORTED_TMUX \
            bash scripts/require-tests.sh \
            cargo test --locked --package tmux-workspace --all-features \
            --test cli the_attach_handoff -- --test-threads=1
        LIBTMUX_TEST_UNSUPPORTED_TMUX="$tmux_binary" \
            LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_UNSUPPORTED_TMUX \
            bash scripts/require-tests.sh \
            cargo test --locked --package tmux-mcp --all-features --lib \
            native_frame_generation_refuses_an_older_replacement \
            -- --test-threads=1
        return
    fi
    if [[ "$source_contract" == "reject-pre-probe" ]]; then
        local current_binary="$build_root/tmux-3.3a-install/bin/tmux"
        if [[ ! -x "$current_binary" ]]; then
            printf 'tmux %s runs behind the 3.3a client, which is not built\n' "$tag" >&2
            return 1
        fi
        LIBTMUX_TEST_PRE_PROBE_TMUX="$tmux_binary" \
            LIBTMUX_TEST_TMUX="$current_binary" \
            LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_PRE_PROBE_TMUX \
            bash scripts/require-tests.sh \
            cargo test --locked --package libtmux --all-features \
            --test daemon_version_check real_tmux_compat_pre_probe_ \
            -- --test-threads=1
        return
    fi
    if [[ "$source_contract" == "reject-boundary" ]]; then
        LIBTMUX_TEST_BOUNDARY_TMUX="$tmux_binary" \
            LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_BOUNDARY_TMUX \
            bash scripts/require-tests.sh \
            cargo test --locked --package libtmux --all-features \
            --test daemon_version_check -- --test-threads=1
        return
    fi

    if [[ "$source_contract" == "feature-boundary" ]]; then
        LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_BROKEN_BREAK_TMUX,LIBTMUX_TEST_MINIMUM_TMUX,LIBTMUX_TEST_NEWER_TMUX \
            LIBTMUX_TEST_BROKEN_BREAK_TMUX="$tmux_binary" \
            LIBTMUX_TEST_MINIMUM_TMUX="$build_root/tmux-3.3a-install/bin/tmux" \
            LIBTMUX_TEST_NEWER_TMUX="$build_root/tmux-3.7b-install/bin/tmux" \
            bash scripts/require-tests.sh \
            cargo test --locked --package libtmux --all-features \
            --test optional_features \
            -- --test-threads=1
        return
    fi

    if [[ "$tag" == "3.3a" ]]; then
        LIBTMUX_TEST_MINIMUM_TMUX="$tmux_binary" \
            LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_MINIMUM_TMUX \
            bash scripts/require-tests.sh \
            cargo test --locked --package libtmux --all-features \
            --test daemon_version_check \
            real_tmux_compat_minimum_daemon_accepts_newer_clients \
            -- --test-threads=1
    fi

    {
        local lane_path
        local selected_version
        local -a feature_environment=()
        if [[ "$tag" == "3.3a" ]]; then
            feature_environment+=("LIBTMUX_TEST_MINIMUM_TMUX=$tmux_binary" "LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_MINIMUM_TMUX")
        elif [[ "$tag" == "3.4" ]]; then
            feature_environment+=("LIBTMUX_TEST_SAFE_BUFFER_TMUX=$tmux_binary" "LIBTMUX_REQUIRE_NATIVE=LIBTMUX_TEST_SAFE_BUFFER_TMUX")
        fi

        if [[ -z "${PATH:-}" ]]; then
            printf 'inherited PATH must be nonempty\n' >&2
            return 1
        fi
        lane_path="$install_prefix/bin:$PATH"
        selected_version="$(LC_ALL=C.UTF-8 PATH="$lane_path" tmux -V)"
        if [[ -n "${lane_version[$tag]:-}" && "$selected_version" != "$expected_version" ]]; then
            printf 'command-local PATH did not select tmux %s\n' "$tag" >&2
            return 1
        fi

        env LC_ALL=C.UTF-8 \
            LIBTMUX_TEST_TMUX="$tmux_binary" \
            PATH="$lane_path" \
            "${feature_environment[@]}" \
            cargo test \
            --locked \
            --workspace \
            --all-targets \
            --all-features
    }

    test_list="$(
        LC_ALL=C.UTF-8 \
            LIBTMUX_TEST_TMUX="$tmux_binary" \
            cargo test \
            --locked \
            --package libtmux \
            --lib \
            --all-features \
            real_tmux_compat_ \
            -- \
            --test-threads=1 \
            --list
    )"
    require_family "$test_list" "real_tmux_compat_format_"
    require_family "$test_list" "real_tmux_compat_aggregate_"
    require_family "$test_list" "real_tmux_compat_projection_"
    require_family "$test_list" "real_tmux_compat_error_"

    LC_ALL=C.UTF-8 \
        LIBTMUX_TEST_TMUX="$tmux_binary" \
        cargo test \
        --locked \
        --package libtmux \
        --lib \
        --all-features \
        real_tmux_compat_ \
        -- \
        --test-threads=1
}

# Supported lanes run the whole workspace against their build. The older
# lanes prove refusal without effects. The crate's version sensitivity is
# not confined to the codec: flags, control mode, and the stderr wording that
# separates a missing target from a refusal all differ by release in
# principle, and asserting them on one version proves nothing about the
# others. The manifest says which lanes run and why each is there.
run_selected
