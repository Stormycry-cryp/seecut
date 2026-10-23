#!/usr/bin/env bash
# GitHub Ubuntu only. Never disable existing swap or change cgroup limits.
set -euo pipefail

swap_bytes=$((8 * 1024 * 1024 * 1024))

init() {
  : "${RUNNER_TEMP:?RUNNER_TEMP is required}"
  memory_dir="$RUNNER_TEMP/seecut-engine-diagnostics/memory"
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

read_active_swap() {
  local active_swap active_name unescaped
  active_swap=$(LC_ALL=C swapon --show=NAME --noheadings --raw) || return 1
  if [[ -n $active_swap ]]; then
    while IFS= read -r active_name; do
      # Raw NAME output hex-escapes whitespace, backslashes and controls.
      # Reject a changed output format before treating any path as inactive.
      unescaped=${active_name//\\x[[:xdigit:]][[:xdigit:]]/}
      if [[ $active_name != /* || $active_name == *'\x00'* ||
        $unescaped == *\\* || $unescaped == *[[:space:][:cntrl:]]* ]]; then
        echo 'Cannot verify active swap names; retaining file and recovery path' >&2
        return 1
      fi
    done <<< "$active_swap"
  fi
  printf '%s\n' "$active_swap"
}

swap_is_active() {
  local swap_file=$1 active_swap=$2 active_name
  [[ -n $active_swap ]] || return 1
  while IFS= read -r active_name; do
    printf -v active_name '%b' "$active_name"
    # A runner directory alias may differ from the name reported by swapon.
    if [[ $active_name == "$swap_file" || $active_name -ef $swap_file ]]; then
      return 0
    fi
  done <<< "$active_swap"
  return 1
}

cleanup() {
  require_runner || return 1
  [[ -f "$memory_dir/swap.path" ]] || return 0
  local swap_file
  swap_file=$(cat "$memory_dir/swap.path")
  if [[ $swap_file != "$RUNNER_TEMP"/seecut-linux-swap.* || ! -f $swap_file || -L $swap_file ]]; then
    echo 'Refusing to clean an unexpected swap path' >&2
    return 1
  fi
  local active_swap
  active_swap=$(read_active_swap) || return 1
  printf 'Temporary swap cleanup path: %s\nActive swap before cleanup:\n%s\n' "$swap_file" "$active_swap"
  if swap_is_active "$swap_file" "$active_swap"; then
    # On failure retain both the file and path, so a retry remains possible.
    printf 'Disabling temporary swap: %s\n' "$swap_file"
    sudo swapoff "$swap_file" || return 1
  fi
  active_swap=$(read_active_swap) || return 1
  printf 'Active swap after cleanup:\n%s\n' "$active_swap"
  if swap_is_active "$swap_file" "$active_swap"; then
    echo 'Temporary swap is still active; retaining file and recovery path' >&2
    return 1
  fi
  # rm may continue to its next operand after a failed unlink. Keep the
  # recovery path until the swap file has actually been removed.
  rm -- "$swap_file" || return 1
  rm -- "$memory_dir/swap.path"
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
  swap_file=$(mktemp "$RUNNER_TEMP/seecut-linux-swap.XXXXXX")
  printf '%s\n' "$swap_file" > "$memory_dir/swap.path"
  chmod 600 "$swap_file"
  dd if=/dev/zero of="$swap_file" bs=1M count=8192 conv=fsync status=none
  sudo mkswap "$swap_file"
  sudo swapon "$swap_file"
  if ! swapon --show --bytes --noheadings --raw --output NAME,SIZE |
    awk -v path="$swap_file" -v size="$((swap_bytes - 4096))" \
      '$1 == path && $2 >= size { found = 1 } END { exit !found }'; then
    echo 'Additional Linux engine build swap was not activated at the expected size' >&2
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

main() {
  init
  case ${1:-} in
    prepare) prepare ;;
    cleanup) cleanup 2>&1 | tee -a "$memory_dir/cleanup.log" ;;
    *) echo 'Usage: ci-linux-memory.sh prepare|cleanup' >&2; return 2 ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
