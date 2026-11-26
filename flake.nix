{
  description = "Taiji-Timo online Tai Chi classes site";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url  = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane/v0.24.0";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, crane, ... }:
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
        # rust-toolchain.toml's Rust: rustc, cargo, clippy, rustfmt and
        # rust-analyzer, the same version as the other projects
        rustOf = p: p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      with pkgs;
      {
        packages.default = callPackage ./nix/package.nix { craneLib = (crane.mkLib pkgs).overrideToolchain rustOf; };

        # Runs the NixOS module in a virtual machine, see nix/test.nix
        checks.module = import ./nix/test.nix { inherit pkgs self; };

        devShells.default = mkShell {
          buildInputs = [
            (rustOf pkgs)
            just
            watchexec
            exiftool # for the pre-commit hook's photo location check
            stripe-cli # `stripe listen` forwards test payments' webhooks to the dev server
          ];
          shellHook = ''
            git config core.hooksPath .githooks
          '';
        };
      }
    );
}
