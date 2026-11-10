#!/bin/sh
# Install (or remove) a launchd agent that runs scripts/trigger.sh every 30 minutes.
#
#   scripts/install-trigger.sh            install and start it
#   scripts/install-trigger.sh uninstall  stop and remove it
#
# launchd runs a missed interval when the Mac wakes, so a laptop that sleeps through the
# evening still starts the day's run when it opens. Log: ~/Library/Logs/paper-trader-trigger.log
set -eu

LABEL=org.paper-trader.trigger
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
SCRIPT="$(cd "$(dirname "$0")" && pwd)/trigger.sh"
LOG="$HOME/Library/Logs/paper-trader-trigger.log"
DOMAIN="gui/$(id -u)"

launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
if [ "${1:-}" = "uninstall" ]; then
  rm -f "$PLIST"
  echo "removed $LABEL"
  exit 0
fi

command -v gh >/dev/null || { echo "install the GitHub CLI and run 'gh auth login' first" >&2; exit 1; }
GH_DIR="$(dirname "$(command -v gh)")"
mkdir -p "$(dirname "$PLIST")" "$(dirname "$LOG")"
cat >"$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array><string>/bin/sh</string><string>$SCRIPT</string></array>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>$GH_DIR:/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
  <key>StartInterval</key><integer>1800</integer>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict>
</plist>
EOF
launchctl bootstrap "$DOMAIN" "$PLIST"
echo "installed $LABEL: runs $SCRIPT every 30 minutes, log $LOG"
