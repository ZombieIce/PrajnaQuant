#!/bin/sh
# Render only. This intentionally does not bootstrap/load the LaunchAgent.
set -eu
PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
LABEL=org.prajnaquant.daily-sync
DEST_DIR=${HOME:?}/Library/LaunchAgents
DEST="$DEST_DIR/$LABEL.plist"
mkdir -p "$DEST_DIR"
cat > "$DEST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>/bin/sh</string><string>$PROJECT_DIR/scripts/daily_sync_schedule.sh</string></array>
  <key>WorkingDirectory</key><string>$PROJECT_DIR</string>
  <key>StartCalendarInterval</key><array>
    <dict><key>Weekday</key><integer>1</integer><key>Hour</key><integer>19</integer><key>Minute</key><integer>30</integer></dict>
    <dict><key>Weekday</key><integer>2</integer><key>Hour</key><integer>19</integer><key>Minute</key><integer>30</integer></dict>
    <dict><key>Weekday</key><integer>3</integer><key>Hour</key><integer>19</integer><key>Minute</key><integer>30</integer></dict>
    <dict><key>Weekday</key><integer>4</integer><key>Hour</key><integer>19</integer><key>Minute</key><integer>30</integer></dict>
    <dict><key>Weekday</key><integer>5</integer><key>Hour</key><integer>19</integer><key>Minute</key><integer>30</integer></dict>
  </array>
  <key>StandardOutPath</key><string>$PROJECT_DIR/data-core/daily-sync.stdout.log</string>
  <key>StandardErrorPath</key><string>$PROJECT_DIR/data-core/daily-sync.stderr.log</string>
  <key>RunAtLoad</key><false/>
</dict></plist>
PLIST
echo "Rendered disabled LaunchAgent configuration: $DEST"
echo "Review it and configure configs/daily-sync.symbols before manually loading it."
