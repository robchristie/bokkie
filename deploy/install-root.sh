#!/usr/bin/env bash
# Run on Nostromo after the reviewed release and private configuration are staged.
set -euo pipefail
test "$(id -u)" -eq 0
stack=/srv/stacks/bokkie
test -f "$stack/release.json"
test -f "$stack/apparmor.profile"
profile=$(python3 - "$stack" <<'PY'
import sys
from pathlib import Path
root = Path(sys.argv[1])
sys.path.insert(0, str(root / 'source/deploy'))
import manage
config = manage.load(root)
if config['name'] != 'bokkie':
    raise ValueError('persistent installer only owns the bokkie deployment')
expected = (manage.POLICY / 'apparmor.profile').read_text().replace(
    'BOKKIE_PROFILE', manage.profile_name(config))
if (root / 'apparmor.profile').read_text() != expected:
    raise ValueError('rendered profile differs from the staged source')
print(manage.profile_name(config))
PY
)
destination="/etc/apparmor.d/$profile"
if [[ -e "$destination" ]]; then
  cmp "$stack/apparmor.profile" "$destination"
else
  install -o root -g root -m 0644 "$stack/apparmor.profile" "$destination"
fi
# No alternate feature discovery: the native parser observes the actual kernel.
apparmor_parser -Q -K --warn=rule-not-enforced --Werror=rule-not-enforced "$destination"
apparmor_parser -r -K --warn=rule-not-enforced --Werror=rule-not-enforced "$destination"
grep -Fx "$profile (enforce)" /sys/kernel/security/apparmor/profiles >/dev/null
unit=/etc/systemd/system/bokkie.service
if [[ -e "$unit" ]]; then
  cmp "$stack/source/deploy/bokkie.service" "$unit"
else
  install -o root -g root -m 0644 "$stack/source/deploy/bokkie.service" "$unit"
fi
systemd-analyze verify "$unit"
systemctl daemon-reload
systemctl enable bokkie.service
# Start/restart is a separate deliberate operation after configuration validation.
