#!/usr/bin/env bash
# GitHub Ubuntu only. Never disable existing swap or change cgroup limits.
set -euo pipefail

swap_bytes=$((8 * 1024 * 1024 * 1024))

init() {
  : "${RUNNER_TEMP:?RUNNER_TEMP is required}"
  memory_dir="$RUNNER_TEMP/seecut-android-memory"
  mkdir -p "$memory_dir"
}

require_runner() {
  if [[ ${GITHUB_ACTIONS:-} != true || ${RUNNER_OS:-} != Linux ]]; then
    echo 'Swap preparation/cleanup requires a GitHub Actions Linux runner' >&2
    return 1
  fi
}

cgroup_path() {
  awk -F: '$1 == "0" { print $3 }' /proc/self/cgroup
}

# Ancestors also constrain the command. Root may omit limit files because
# it is unlimited. A non-root group without readable swap limits is unknown.
inspect_cgroup() {
  local root=$1 path=$2 enforce=${3:-false} metric value current
  if [[ $path != /* || $path == *..* ]]; then
    echo 'No observable cgroup v2 path; effective swap capacity is unknown' >&2
    return 1
  fi
  while :; do
    for metric in memory.max memory.high memory.current memory.peak memory.events memory.swap.max memory.swap.current; do
      if [[ -r "$root${path%/}/$metric" ]]; then
        printf '%s: ' "$root${path%/}/$metric"
        cat "$root${path%/}/$metric"
      fi
    done
    if [[ $enforce == true ]]; then
      if [[ -r "$root${path%/}/memory.swap.max" ]]; then
        value=$(cat "$root${path%/}/memory.swap.max")
        if [[ $value != max ]]; then
          current=$(cat "$root${path%/}/memory.swap.current")
          if [[ ! $value =~ ^[0-9]+$ || ! $current =~ ^[0-9]+$ ]] || (( value - current < swap_bytes )); then
            echo "Insufficient cgroup swap headroom at $path; need 8 GiB" >&2
            return 1
          fi
        fi
      elif [[ $path != / ]]; then
        echo "Cannot verify cgroup swap limit at $path" >&2
        return 1
      fi
    fi
    [[ $path == / ]] && break
    path=$(dirname "$path")
  done
}

cleanup() {
  require_runner || return 1
  [[ -f "$memory_dir/swap.path" ]] || return 0
  local swap_file
  swap_file=$(cat "$memory_dir/swap.path")
  if [[ $swap_file != "$RUNNER_TEMP"/seecut-android-swap.* || ! -f $swap_file || -L $swap_file ]]; then
    echo 'Refusing to clean an unexpected swap path' >&2
    return 1
  fi
  local active_swap
  active_swap=$(swapon --show --noheadings --raw --output NAME) || return 1
  if grep -Fxq -- "$swap_file" <<< "$active_swap"; then
    # On failure retain both the file and path, so a retry remains possible.
    sudo swapoff "$swap_file" || return 1
  fi
  rm -- "$swap_file" "$memory_dir/swap.path"
}

prepare() (
  require_runner
  exec > >(tee -a "$memory_dir/prepare.log") 2>&1
  if [[ -e "$memory_dir/swap.path" ]]; then
    echo 'Swap state already exists; clean it before preparing again' >&2
    exit 1
  fi
  trap 'status=$?; if (( status != 0 )); then cleanup || true; fi' EXIT
  inspect_cgroup /sys/fs/cgroup "$(cgroup_path)" true
  local available_kib swap_file
  available_kib=$(df -Pk "$RUNNER_TEMP" | awk 'NR == 2 { print $4 }')
  if [[ ! $available_kib =~ ^[0-9]+$ ]] || (( available_kib < (8 + 20) * 1024 * 1024 )); then
    echo 'Insufficient disk space for 8 GiB swap and 20 GiB build reserve' >&2
    df -h "$RUNNER_TEMP"
    exit 1
  fi
  swap_file=$(mktemp "$RUNNER_TEMP/seecut-android-swap.XXXXXX")
  printf '%s\n' "$swap_file" > "$memory_dir/swap.path"
  chmod 600 "$swap_file"
  dd if=/dev/zero of="$swap_file" bs=1M count=8192 conv=fsync status=none
  sudo mkswap "$swap_file"
  sudo swapon "$swap_file"
  if ! swapon --show --bytes --noheadings --raw --output NAME,SIZE |
    awk -v path="$swap_file" -v size="$((swap_bytes - 4096))" \
      '$1 == path && $2 >= size { found = 1 } END { exit !found }'; then
    echo 'Additional Android build swap was not activated at the expected size' >&2
    exit 1
  fi
  available_kib=$(df -Pk "$RUNNER_TEMP" | awk 'NR == 2 { print $4 }')
  if [[ ! $available_kib =~ ^[0-9]+$ ]] || (( available_kib < 20 * 1024 * 1024 )); then
    echo 'Less than 20 GiB build reserve remains after creating swap' >&2
    exit 1
  fi
  swapon --show --bytes
  free -h
  df -h "$RUNNER_TEMP"
)

snapshot() {
  date -u '+%Y-%m-%dT%H:%M:%SZ'
  free -h || true
  df -h . "$RUNNER_TEMP" || true
  swapon --show --bytes || true
  inspect_cgroup /sys/fs/cgroup "$(cgroup_path)" || true
  # Executable names only: argv/environment may contain credentials.
  ps -eo pid,ppid,rss,comm --sort=-rss | head -n 21 || true
}

monitor() {
  local sleeper=''
  trap 'if [[ -n $sleeper ]]; then kill "$sleeper" 2>/dev/null || true; fi; exit 0' TERM INT
  while :; do
    snapshot | tee -a "$memory_dir/resources.log"
    sleep 30 &
    sleeper=$!
    wait "$sleeper" || true
  done
}

run_build() (
  [[ ${1:-} != -- ]] || shift
  (( $# > 0 )) || { echo 'run requires a command' >&2; exit 2; }
  inspect_cgroup /sys/fs/cgroup "$(cgroup_path)" true 2>&1 | tee -a "$memory_dir/prepare.log"
  # Stream samples to the job log too: a runner shutdown may prevent upload.
  monitor &
  # EXIT runs after function locals unwind on some Bash versions.
  monitor_pid=$!
  finish() {
    local status=$?
    trap - EXIT
    kill "$monitor_pid" 2>/dev/null || true
    wait "$monitor_pid" || true
    snapshot 2>&1 | tee -a "$memory_dir/resources.log"
    printf 'exit_code=%s\n' "$status" > "$memory_dir/result.txt"
    exit "$status"
  }
  trap finish EXIT
  trap 'exit 143' TERM
  trap 'exit 130' INT
  "$@"
)

main() {
  init
  case ${1:-} in
    prepare) prepare ;;
    run) shift; run_build "$@" ;;
    cleanup) cleanup 2>&1 | tee -a "$memory_dir/cleanup.log" ;;
    *) echo 'Usage: ci-android-memory.sh prepare|run -- COMMAND...|cleanup' >&2; return 2 ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
