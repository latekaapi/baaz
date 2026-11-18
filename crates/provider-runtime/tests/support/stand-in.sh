#!/bin/sh
# baaz-rt-stand-in: the stand-in child (run as `/bin/sh stand-in.sh <mode>`) for provider-runtime's fault tests.
# Never a real CLI: each mode misbehaves the way a child can.
case "$1" in
  # A grandchild that ignores SIGTERM and lives until killed.
  grand) trap '' TERM; while :; do sleep 1; done ;;
  # Ignores SIGTERM, starts a grandchild that does too, prints its pid.
  stubborn) trap '' TERM; /bin/sh "$0" grand & echo "$!"; while :; do sleep 1; done ;;
  # A good line, invalid UTF-8, a line over 8 MB, a good line, then exit.
  garbage)
    printf 'first\n\377\376{"bad":1}\n'
    head -c 9000000 /dev/zero | tr '\000' 'x'
    printf '\nlast\n'
    ;;
  # Never reads its stdin.
  deaf) while :; do sleep 1; done ;;
  # Echoes each stdin line.
  echo) while read -r line; do echo "$line"; done ;;
  *) echo "unknown mode: $1" >&2; exit 2 ;;
esac
