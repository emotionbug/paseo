#!/bin/sh
# Arguments are private, validated paths supplied by DesktopUpdater, never shell code.
set -eu
parent=$1
target=$2
staged=$3
backup=$4
kind=$5
log=$6
exec >>"$log" 2>&1
attempt=0
while kill -0 "$parent" 2>/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 120 ]; then
    echo 'Update aborted: the application did not exit within 120 seconds.'
    exit 1
  fi
  sleep 1
done
if [ ! -e "$target" ] || [ ! -e "$staged" ] || [ -e "$backup" ]; then
  echo 'Update aborted: installation paths changed after validation.'
  exit 1
fi
mv "$target" "$backup"
if ! mv "$staged" "$target"; then
  mv "$backup" "$target"
  echo 'Update failed; restored the previous installation.'
  exit 1
fi
if [ "$kind" = macos ]; then
  if ! /usr/bin/open "$target"; then
    mv "$target" "$staged"
    mv "$backup" "$target"
    /usr/bin/open "$target" || true
    echo 'Update launch failed; restored the previous installation.'
    exit 1
  fi
else
  # AppImage uses its original path so shortcuts and CLI symlinks remain valid.
  "$target" </dev/null >>"$log" 2>&1 &
  launched=$!
  sleep 1
  if ! kill -0 "$launched" 2>/dev/null; then
    if ! wait "$launched"; then
      mv "$target" "$staged"
      mv "$backup" "$target"
      "$target" </dev/null >>"$log" 2>&1 &
      echo 'Update launch failed; restored the previous installation.'
      exit 1
    fi
  fi
fi
rm -rf "$backup"
rmdir "$(dirname "$backup")" || true
echo 'Update replacement completed.'
