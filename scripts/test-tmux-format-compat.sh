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

run_lane() {
    local tag="$1"
    local expected_commit="$2"
    local expected_version="$3"
    local source_root="$build_root/tmux-$tag-source"
    local install_prefix="$build_root/tmux-$tag-install"
    local tmux_binary="$install_prefix/bin/tmux"
    local tag_type
    local peeled_commit
    local checkout_commit
    local installed_version
    local test_list

    git clone \
        --branch "$tag" \
        --depth 1 \
        --single-branch \
        https://github.com/tmux/tmux.git \
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

    checkout_commit="$(git -C "$source_root" rev-parse HEAD)"
    if [[ "$checkout_commit" != "$expected_commit" ]]; then
        printf 'tmux %s checkout commit does not match\n' "$tag" >&2
        return 1
    fi

    (
        cd "$source_root"
        sh autogen.sh
        ./configure --prefix="$install_prefix"
        make -j"$(nproc)"
        make install
    )

    if [[ ! -x "$tmux_binary" ]]; then
        printf 'tmux %s did not install an executable\n' "$tag" >&2
        return 1
    fi

    installed_version="$("$tmux_binary" -V)"
    if [[ "$installed_version" != "$expected_version" ]]; then
        printf 'tmux %s version output does not match\n' "$tag" >&2
        return 1
    fi

    {
        local lane_path
        local selected_version

        if [[ -z "${PATH:-}" ]]; then
            printf 'inherited PATH must be nonempty\n' >&2
            return 1
        fi
        lane_path="$install_prefix/bin:$PATH"
        selected_version="$(LC_ALL=C.UTF-8 PATH="$lane_path" tmux -V)"
        if [[ "$selected_version" != "$expected_version" ]]; then
            printf 'command-local PATH did not select tmux %s\n' "$tag" >&2
            return 1
        fi

        LC_ALL=C.UTF-8 \
            LIBTMUX_TEST_TMUX="$tmux_binary" \
            PATH="$lane_path" \
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

# Every lane runs the whole workspace against the tmux it built. The crate's
# version sensitivity is not confined to the codec: flags, control mode, and
# the stderr wording that separates a missing target from a refusal all differ
# by release in principle, and asserting them on one version proves nothing
# about the others.
run_lane \
    "3.2a" \
    "3b929f332aafa7f1080eacc31feb11ffbb1d1841" \
    "tmux 3.2a" \

# 3.4 and 3.5a are the releases that wrapped command output in
# VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH. They are the reason the codec has a second
# dialect at all, so leaving them out of CI would leave the decoder that
# handles them unexercised on every platform but a developer's own.
run_lane \
    "3.4" \
    "9ae69c3795ab5ef6b4d760f6398cd9281151f632" \
    "tmux 3.4" \

run_lane \
    "3.5a" \
    "549c35b06165f6ae023115eb76f83f2cbf945395" \
    "tmux 3.5a" \

# The final patch of each series rather than its first: 3.2a, 3.5a, 3.6b, and
# 3.7b are what a distribution ships and what a user runs. 3.4 is the
# exception because that series has no later patch.
run_lane \
    "3.6b" \
    "0623d1e968423ad0c192e0d8debf1258671063d5" \
    "tmux 3.6b" \

run_lane \
    "3.7b" \
    "e802909de06012a4df6209d55e86487c56223163" \
    "tmux 3.7b" \
