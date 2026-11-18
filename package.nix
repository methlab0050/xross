# xross as a Nix package, with its login session for display managers. Used by
# flake.nix (`nix build`, and the NixOS module that adds the session).
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  wayland,
  libxkbcommon,
  libinput,
  seatd,
  systemdLibs,
  libdrm,
  libdisplay-info,
  libgbm,
  mesa,
  libGL,
  libglvnd,
  vulkan-loader,
  pixman,
  fontconfig,
  freetype,
  xwayland,
  xrdb,
  dbus,
}:

let
  cargoToml = lib.importTOML ./Cargo.toml;
  # Loaded with dlopen() at runtime (EGL/GL, Wayland and xkbcommon for winit).
  runtimeLibs = [
    wayland
    libxkbcommon
    libGL
    libglvnd
    vulkan-loader
    mesa
  ];
in
rustPlatform.buildRustPackage {
  pname = "xross";
  inherit (cargoToml.package) version;

  # Only what the build needs (not target/, the dev shell, or .cargo/config.toml,
  # which links with mold for development).
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
      ./resources
    ];
  };

  cargoLock = {
    lockFile = ./Cargo.lock;
    # Smithay comes from git; its hash, for the fetched checkout.
    outputHashes = {
      "smithay-0.7.0" = "sha256-NG49dc+dCPGRoxeaEUkQ/8smI4JWqnCZVzEAWWvanlc=";
    };
  };

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];

  buildInputs = [
    wayland
    libxkbcommon
    libinput
    seatd
    systemdLibs
    libdrm
    libdisplay-info
    libgbm
    mesa
    libGL
    libglvnd
    pixman
    fontconfig
    freetype
  ];

  # The tests run in the dev shell (some need fonts and icon themes, which the build
  # sandbox doesn't have).
  doCheck = false;

  postInstall = ''
    install -Dm644 resources/xross.desktop $out/share/wayland-sessions/xross.desktop
    install -Dm644 resources/default-config.toml $out/share/doc/xross/default-config.toml
    # Tools xross runs itself. The terminal (foot by default) and screen locker come
    # from the system.
    wrapProgram $out/bin/xross \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs} \
      --prefix PATH : ${
        lib.makeBinPath [
          xwayland
          xrdb
          dbus
        ]
      }
  '';

  passthru.providedSessions = [ "xross" ];

  meta = {
    description = "A stacking Wayland compositor with a 2D workspace grid, an overview and XWayland";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "xross";
  };
}
