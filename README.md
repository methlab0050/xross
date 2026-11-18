# xross

A stacking Wayland compositor with XWayland, written in Rust on
[Smithay](https://github.com/Smithay/smithay).

Windows float by default and snap into a 2×2 column grid. Workspaces live on a 2D
grid that tidies itself up. There's a zoomed-out overview, alt-tab with live
thumbnails, and spring animations that follow your fingers on a touchpad.

## Features

- **Snapping:** Mod+Left/Right for halves, Mod+Up to maximize, Mod+Down to restore.
  Press Mod+Right then Mod+Up quickly for the upper-right quadrant, and so on. Snapping
  onto an occupied slot swaps the two windows.
- **2D workspaces:** move in any direction from a workspace with windows. Empty rows
  and columns close up once you leave them, and groups of workspaces that get cut off
  slide back next to the rest, so none are ever unreachable.
- **Overview:** shows every workspace with windows. Click or press Enter to go to one,
  use the arrow keys or scroll to move around, and drag windows between workspaces or
  onto a new empty slot. Start typing to search with your launcher
  (`overview.launcher`, e.g. fuzzel): it's started in the background when the overview
  opens, so it appears instantly with what you typed.
- **Alt-tab:** a quick tap flips between the last two windows. Hold to see tiles, then
  pick with Tab/Shift+Tab, the arrow keys or the mouse; release the modifier to switch.
- **Touchpad gestures:** 3-finger swipe switches workspaces, 4-finger swipe up/down
  opens and closes the overview, and 2 or 3 fingers pan around the overview.
- **Looks:** server-side title bars (only for apps that want them), rounded corners,
  soft shadows, and open/close animations.
- **Daily-driver protocols:** layer-shell (bars, launchers, wallpapers), session lock
  (swaylock), idle inhibit, xdg-activation, screencopy (grim, wf-recorder), fractional
  scaling, cursor shape, presentation time, and dmabuf.
- **XWayland**, with an option to show X11 apps at native resolution so they stay sharp
  under fractional scaling.

## Keybinds

Every keybind is listed in the default config (see [Configuration](#configuration)),
under `[binds.prod]` and `[binds.dev]`. The defaults are below.

There are two profiles. **prod** (Super-based) is the default on real hardware. **dev**
(Alt-based) is the default when running nested inside another desktop, which usually
grabs Super.

| Action | prod | dev |
|---|---|---|
| Snap / maximize / restore | Super+Arrows | Alt+Arrows |
| Switch workspace | Ctrl+Super+Arrows | Shift+Alt+Arrows |
| Move window to workspace | Ctrl+Shift+Super+Arrows | Ctrl+Shift+Alt+Arrows |
| Window switcher | Super+Tab, Super+Shift+Tab | Alt+Tab, Alt+Shift+Tab |
| Overview | tap Super | tap Shift+Alt |
| Move / resize a window | Super+drag, Super+right-drag | Alt+drag, Alt+right-drag |
| Terminal | Ctrl+` | Ctrl+` |
| Quit | Ctrl+Alt+Backspace | Ctrl+Alt+Backspace |

`--binds dev|prod` or `general.binds` in the config picks the profile. Mouse
move/resize uses the profile's modifier. Ctrl+Alt+F1..F12 (switching VTs) is always
built in.

To change binds, add entries to your config's table for the profile. The same combo
replaces the default, `"none"` removes a default (the keys then go to apps), and
modifiers alone make a tap:

```toml
[binds.prod]
"Super+q" = "close"
"Super+Return" = "spawn foot"
"Super+d" = "spawn fuzzel"
"Ctrl+Super+Left" = "none"
"Super" = "overview"
```

## Configuration

Your config lives at `~/.config/xross/config.toml` (or under `$XDG_CONFIG_HOME`) and
reloads when you save it. It only needs the settings you want to change; everything
else keeps its default.

Every option is in the default config, with its default value and what it does. The
file is built into xross ([`resources/default-config.toml`](resources/default-config.toml)),
and xross keeps a read-only copy at `~/.local/state/xross/default-config.toml`,
rewritten whenever the defaults change. The options cover output scale, XWayland
scaling, gaps, animations, the overview, touchpad, appearance (corner radius, shadows,
font, icon theme), startup commands, the terminal command and keybinds.

Logs go to `~/.local/state/xross/xross.log` (the previous run's log is kept as
`xross.log.old`). Problems in the config file are logged there, and the last good
config stays in use.

## Installing on NixOS (flake)

Add the input to your system flake:

```nix
inputs.xross = {
  url = "git+file:///path/to/xross"; # or a git URL
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Then, in a NixOS module:

```nix
imports = [ inputs.xross.nixosModules.default ];
programs.xross.enable = true;
```

This adds an **xross** session to your display manager (in GDM, it's under the gear
button) and puts `xross` on your PATH. xross is built with your system's nixpkgs, so it
uses the same Mesa as your graphics driver.

xross needs Xwayland, and the package includes it. You provide the rest from your own
config:

- a terminal (`foot` by default; change it with `general.terminal`)
- a screen locker, if you want one. swaylock also needs
  `security.pam.services.swaylock = {};` to check your password.
- anything else you like: bars, launchers, wallpaper tools, idle daemons

To pick up new commits, run `nix flake update xross` in your system flake and rebuild.

Elsewhere, `nix build` builds the package and `nix run` runs it.

## Development

The dev environment uses [devenv](https://devenv.sh):

```sh
devenv shell
nested        # run in a window inside your current desktop (dev binds)
btty          # run on DRM/KMS from a text console (prod binds)
t             # run the tests (cargo nextest)
```

While xross runs, `xross msg ACTION` controls it, for example
`xross msg workspace right`, `xross msg overview` or `xross msg screenshot out.png`.
That's handy for scripting and testing.

## License

MIT; see [LICENSE](LICENSE). Some files started as copies of Smithay's examples, which
are also MIT; see [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES).
