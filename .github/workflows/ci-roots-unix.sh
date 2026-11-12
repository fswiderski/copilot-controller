#!/usr/bin/env bash
set -euo pipefail
umask 077

case "$RUNNER_OS" in
  Linux)
    parent="$(cd "$HOME" && pwd -P)"
    filesystem="$(findmnt -rn -o FSTYPE --target "$parent")"
    case "$filesystem" in
      ext2|ext3|ext4|xfs|btrfs) ;;
      *) echo "Unsupported Linux fixture filesystem: $filesystem" >&2; exit 1 ;;
    esac
    ;;
  macOS)
    # A runner's home may carry macOS's default extended ACL.
    parent="$(cd /Users/Shared && pwd -P)"
    if ! df -T apfs "$parent" >/dev/null; then
      echo "Fixture filesystem must be local APFS" >&2
      exit 1
    fi
    filesystem=apfs
    ;;
  *) echo "Unsupported Unix runner: $RUNNER_OS" >&2; exit 1 ;;
esac
echo "Fixture filesystem: $filesystem"

CI_FIXTURE_PARENT="$parent" python3 - <<'PY'
import os
import stat
import subprocess
import sys

parent = os.environ["CI_FIXTURE_PARENT"]
run_id = os.environ["GITHUB_RUN_ID"]
attempt = os.environ["GITHUB_RUN_ATTEMPT"]
if not all(value.isascii() and value.isdecimal() for value in (run_id, attempt)):
    raise SystemExit("Invalid run ID or attempt")
if not os.path.isabs(parent) or os.path.realpath(parent) != parent:
    raise SystemExit("Fixture parent must be an absolute, resolved path")
if "\n" in parent or "\r" in parent:
    raise SystemExit("Invalid fixture parent")


def check_ancestors(path):
    while True:
        info = os.lstat(path)
        if not stat.S_ISDIR(info.st_mode):
            raise SystemExit(f"Not a real directory: {path}")
        writable = info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
        root_sticky = info.st_uid == 0 and info.st_mode & stat.S_ISVTX
        if writable and not root_sticky:
            raise SystemExit(f"Unsafe writable ancestor: {path}")
        if sys.platform == "darwin":
            listing = subprocess.run(
                ["/bin/ls", "-lde", path],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout
            if len(listing.splitlines()) != 1:
                raise SystemExit(f"Extended ACL on fixture ancestor: {path}")
        next_path = os.path.dirname(path)
        if next_path == path:
            break
        path = next_path


check_ancestors(parent)
root = os.path.join(parent, f".controller-ci-{run_id}-{attempt}")
os.mkdir(root, 0o700)
target = os.path.join(root, "target")
fixtures = os.path.join(root, "fixtures")
for path in (target, fixtures):
    os.mkdir(path, 0o700)
for path in (root, target, fixtures):
    info = os.lstat(path)
    if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise SystemExit(f"Fixture directory is not private: {path}")
    check_ancestors(path)

exports = {
    "CARGO_TARGET_DIR": target,
    "COPILOT_CONTROLLER_TEST_ROOT": fixtures,
    "COPILOT_PRIVATE_ROOT_FIXTURES": fixtures,
    "COPILOT_FIRMWARE_FIXTURE_ROOT": fixtures,
    "PORTABLE_TEST_FIXTURES": fixtures,
    "SESSION_ENGINE_SAFE_FIXTURE_ROOT": fixtures,
    "TMPDIR": fixtures,
    "TMP": fixtures,
    "TEMP": fixtures,
}
with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as env_file:
    for name, value in exports.items():
        env_file.write(f"{name}={value}\n")
print(f"Build root: {target}")
print(f"Private fixture root: {fixtures}")
PY
