# Spout sender

`spout-sender.exe` hands the game's frames to a streaming program on Windows,
on the GPU. The game always renders offscreen. The settings tab's single
**OBS 输出** switch controls this sender (`src/relay.js`):

```text
spout-sender.exe <shell-pid> [--name <sender>]
stdin   "<id> <nt-handle> <width> <height>"   a frame Electron lent the shell
stdout  "<id> ok" | "<id> err <what>"          once the copy has run on the GPU
```

It opens each frame's texture by its handle on a Direct3D 11 device of its own,
copies it into one texture it shares under Spout's sender name (`Washizu`), and
answers; the shell then gives the frame back to Chromium. A Spout receiver —
in OBS, the Spout2 plugin's *Spout2 Capture* source — reads that texture
directly. The sender list, the shared memory a receiver reads, the access
mutex and the frame count follow the Spout2 SDK
(<https://github.com/leadedge/Spout2>, BSD-2-Clause).

## Building

The desktop workflow builds it on Windows with MSVC, and `scripts/package.mjs`
puts it in the application's `resources/spout/`. By hand, from a Developer
Command Prompt:

```bat
cl /nologo /O2 /EHsc /MT /W3 spout-sender.cpp /Fe:build\spout-sender.exe
```

In OBS on Windows, install the Spout2 plugin and add a **Spout2 Capture**
source. Change an old source's sender from **Akagi** to **Washizu** for the
original account. Other profiles use `Washizu <platform> <stable-id>`;
renaming an account does not change its sender, and different stable ids do
not collide. Frames are 2560 × 1440; scale them in OBS as needed.

Without the Windows sender, `obs_available` is false and enabling OBS output
fails explicitly. The offscreen game still renders for the dock. There is no
HTTP/MJPEG/browser-source output path.
