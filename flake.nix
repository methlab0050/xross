{
  description = "xross: a stacking Wayland compositor with a 2D workspace grid";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        xross = pkgs.callPackage ./package.nix { };
        default = xross;
      });

      overlays.default = final: _prev: { xross = final.callPackage ./package.nix { }; };

      # In a NixOS configuration: import this module and set `programs.xross.enable = true;`.
      # The package is built with the system's own nixpkgs, so the Mesa that xross loads
      # matches the system's graphics driver.
      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.xross;
        in
        {
          options.programs.xross = {
            enable = lib.mkEnableOption "xross, with a login session for display managers";
            package = lib.mkOption {
              type = lib.types.package;
              default = pkgs.callPackage ./package.nix { };
              defaultText = lib.literalExpression "pkgs.callPackage xross/package.nix { }";
              description = "The xross package.";
            };
          };

          config = lib.mkIf cfg.enable {
            services.displayManager.sessionPackages = [ cfg.package ];
            # `xross msg ...` from a terminal.
            environment.systemPackages = [ cfg.package ];
          };
        };
    };
}
