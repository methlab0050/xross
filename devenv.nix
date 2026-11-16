{
  pkgs,
  lib,
  config,
  ...
}:

let
  # Libraries that winit / EGL / xkbcommon dlopen() at runtime.
  runtimeLibs = with pkgs; [
    wayland
    libxkbcommon
    libGL
    libglvnd
    vulkan-loader
    mesa
  ];
in
{
  packages = with pkgs; [
    git
    pkg-config
    clang
    mold

    # build inputs for smithay
    wayland
    wayland-protocols
    wayland-scanner
    libxkbcommon
    libinput
    seatd # libseat
    systemdLibs # libudev
    libdrm
    libgbm
    mesa
    libGL
    libglvnd
    pixman
    fontconfig
    freetype

    # runtime / test clients
    xwayland
    foot
    xterm
    xeyes
    mesa-demos
    weston
    wayland-utils
    wev
    wl-clipboard
    xclip
    grim
    slurp
    fuzzel

    # dev tools
    cargo-nextest
    cargo-watch
    just
  ];

  languages.rust = {
    enable = true;
    channel = "stable";
    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      "rust-analyzer"
      "rust-src"
    ];
  };

  env.LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
  env.RUST_LOG = "xross=debug,smithay=warn";
  env.RUST_BACKTRACE = "1";

  scripts.nested.exec = ''cargo run -- --backend winit "$@"'';
  scripts.tty.exec = ''cargo run --release -- --backend udev "$@"'';
  scripts.t.exec = ''cargo nextest run "$@"'';

  enterShell = ''
    echo "xross devenv: $(rustc --version)"
    echo "  nested  - run nested in a winit window (dev binds)"
    echo "  tty     - run on DRM/KMS from a VT (prod binds)"
    echo "  t       - run tests"
  '';

  enterTest = ''
    cargo nextest run
  '';
}
