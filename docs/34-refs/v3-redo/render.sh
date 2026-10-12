#!/bin/zsh
cd "$(dirname "$0")"; mkdir -p out
for f in html/*.html; do n=$(basename $f .html)
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --hide-scrollbars --force-device-scale-factor=2 --window-size=1440,900 --virtual-time-budget=4000 --screenshot=out/$n.png "file://$PWD/$f" 2>/dev/null
done; ls out
