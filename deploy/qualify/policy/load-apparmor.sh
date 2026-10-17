#!/bin/sh
# Runs inside the short-lived loader container that qualify.sh starts with
# only CAP_MAC_ADMIN. It loads, replaces or removes exactly one profile,
# bokkie-qualify-b, and checks that no other loaded profile changed.
#
# Usage: load-apparmor.sh add|replace|remove <sha256 of the policy file>
set -eu

name=bokkie-qualify-b
policy=/policy/bokkie-qualify-b.apparmor
operation=$1
expected=$2

actual=$(sha256sum "$policy" | cut -d' ' -f1)
[ "$actual" = "$expected" ] || { echo "policy digest differs: $actual" >&2; exit 1; }

parse() {
  /usr/sbin/apparmor_parser -f /apparmor -K --kernel-features /apparmor/features \
    --warn=rule-not-enforced --Werror=rule-not-enforced "$@" "$policy"
}
[ "$(parse -N)" = "$name" ] || { echo "policy defines unexpected profiles" >&2; exit 1; }

others() { grep -v "^$name " /apparmor/profiles | sort; }
before=$(others)
case "$operation" in
  add) parse -a ;;
  replace) parse -r ;;
  remove) parse -R ;;
  *) echo "unknown operation $operation" >&2; exit 2 ;;
esac
after=$(others)
[ "$before" = "$after" ] || { echo "unrelated profile inventory changed" >&2; exit 1; }
state=$(grep "^$name " /apparmor/profiles || true)
echo "{\"operation\":\"$operation\",\"profile\":\"$name\",\"sha256\":\"$actual\",\"state\":\"$state\",\"others_unchanged\":true}"
