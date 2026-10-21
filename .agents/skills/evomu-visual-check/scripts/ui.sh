#!/bin/sh
# Minimal Accessibility helpers for driving ONE app window (the EVE client). See ../SKILL.md for rules.
# Usage:
#   ui.sh bounds <process>            -> "x y w h" of the process's front window (points)
#   ui.sh focus  <process>
#   ui.sh type   <process> <text>
#   ui.sh key    <process> <return|tab|escape|space|up|down|left|right|delete>
#   ui.sh click  <process> <x> <y>    (window-relative points)
set -e
cmd="$1"; proc="$2"
[ -n "$cmd" ] && [ -n "$proc" ] || { sed -n '2,9p' "$0" >&2; exit 2; }
osa() { osascript -e "$1"; }
case "$cmd" in
  bounds)
    osa "tell application \"System Events\" to tell process \"$proc\"
           set p to position of front window
           set s to size of front window
           return (item 1 of p as text) & \" \" & (item 2 of p as text) & \" \" & (item 1 of s as text) & \" \" & (item 2 of s as text)
         end tell" ;;
  focus)
    osa "tell application \"System Events\" to set frontmost of process \"$proc\" to true" ;;
  type)
    osa "tell application \"System Events\"
           set frontmost of process \"$proc\" to true
           delay 0.2
           keystroke \"$3\"
         end tell" ;;
  key)
    case "$3" in
      return) code=36;; tab) code=48;; escape) code=53;; space) code=49;; delete) code=51;;
      left) code=123;; right) code=124;; down) code=125;; up) code=126;;
      *) echo "unknown key $3" >&2; exit 2;;
    esac
    osa "tell application \"System Events\"
           set frontmost of process \"$proc\" to true
           delay 0.2
           key code $code
         end tell" ;;
  click)
    set -- $(sh "$0" bounds "$proc") "$3" "$4"
    x=$(( $1 + $5 )); y=$(( $2 + $6 ))
    osa "tell application \"System Events\"
           set frontmost of process \"$proc\" to true
           delay 0.2
           click at {$x, $y}
         end tell" ;;
  *) echo "unknown command $cmd" >&2; exit 2;;
esac
