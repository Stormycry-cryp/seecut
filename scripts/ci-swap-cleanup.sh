# Shared cleanup for a single swap file recorded by an engine or Android CI job.
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

cleanup_ci_swap() {
  require_runner || return 1
  [[ -f "$memory_dir/swap.path" ]] || return 0
  local swap_prefix=$1 swap_file
  swap_file=$(cat "$memory_dir/swap.path")
  if [[ $swap_file != "$RUNNER_TEMP/$swap_prefix".* || ! -f $swap_file || -L $swap_file ]]; then
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
