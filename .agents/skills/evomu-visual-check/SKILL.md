---
name: evomu-visual-check
description: >-
  See and drive the real EVE V24.01 client on this Mac: take window screenshots and read them, and send keyboard/mouse
  input to the client window through macOS Accessibility. Use whenever you launch the real client against a local
  EvoMu server (T-6 harness, real-client journeys, debugging what the client actually shows or does).
---

# Real-client visual verification and input on macOS

The user has granted Antigravity **Screen Recording** and **Accessibility** permissions. Any agent or subagent can:
- capture a window with `screencapture` and **look at it with `read_file`** (images render in the file viewer);
- read window positions and send keystrokes and clicks with `osascript` / System Events.

Use this to verify what the real client shows (login, character selection, station UI, overview, dialogs, error
popups, tracebacks, disconnects) and to step through journeys the harness can't drive from code.

## Screenshots
1. Find the window id (CoreGraphics; works without Accessibility):
   ```bash
   swift .agents/skills/evomu-visual-check/scripts/winlist.swift        # id<TAB>owner<TAB>title for all app windows
   swift .agents/skills/evomu-visual-check/scripts/winlist.swift EVE    # filter by owner/title substring
   ```
2. Capture **only that window**:
   ```bash
   .agents/skills/evomu-visual-check/scripts/shot.sh <window-id> <out.png>   # screencapture -x -o -l<id>
   .agents/skills/evomu-visual-check/scripts/shot.sh <out.png>               # first window matching "EVE"
   ```
3. `read_file <out.png>`, then state what you see versus what the step expects.

## Input (Accessibility)
```bash
.agents/skills/evomu-visual-check/scripts/ui.sh bounds  "<process>"            # x y w h of its front window
.agents/skills/evomu-visual-check/scripts/ui.sh focus   "<process>"            # bring the client to the front
.agents/skills/evomu-visual-check/scripts/ui.sh type    "<process>" "text"     # type text into the focused field
.agents/skills/evomu-visual-check/scripts/ui.sh key     "<process>" return     # return | tab | escape | space | up/down/left/right
.agents/skills/evomu-visual-check/scripts/ui.sh click   "<process>" <x> <y>    # click at window-relative x,y (points)
```
Find the process name with `osascript -e 'tell application "System Events" to get name of every process whose background only is false'`.
Window-relative coordinates are in points (screenshots are in pixels; on Retina divide pixel coords by 2).
Always take a screenshot after an input step to confirm it took effect; never chain inputs blind.

## Rules
- **Evidence, not decoration.** A screenshot supports a journey claim only together with server and client logs from
  the same run. Name files after the step (`launch_login_03_charsel.png`) and record the expected state next to each
  in the harness `result.json` → `evidence`. Visual checks never replace asserting server state in tests.
- **Client imagery never goes into git.** Client screenshots are client-derived content and the repo is public. Keep
  them under the harness output dir or `.evomu-reviews/` (both gitignored).
- **Only the client.** Capture and send input only to the EVE client (and its launcher/crash dialogs). Never capture the
  full screen, never type into, click on, move, close or read other apps (browser, mail, terminals, password managers,
  Antigravity itself). The user may be working at the same time: focus the client only for the duration of an input
  step and keep steps short.
- **No credentials of the user.** Use only EvoMu demo/test accounts. Never type the user's real EVE/CCP credentials,
  and never point the client at CCP's live servers.
- `screencapture`, `swift` and `osascript` are normal system tools. If Santa or a permission prompt blocks one,
  report it; never bypass.
