{
  description = "Taiji-Timo online Tai Chi classes site";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url  = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    {
      # Runs the site as a systemd service, see nix/module.nix
      nixosModules.default = import ./nix/module.nix;
    }
    // flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
      in
      with pkgs;
      {
        packages.default = callPackage ./nix/package.nix { };

        devShells.default = mkShell {
          buildInputs = [
            rust-bin.stable.latest.default
            just
            watchexec
          ];
          shellHook = ''
            git config core.hooksPath .githooks
          '';
        };
      }
    );
}
