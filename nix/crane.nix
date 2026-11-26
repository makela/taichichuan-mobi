# Crane for builds outside the flake (e.g. the NixOS module imported from a
# clone): the same version as the flake's crane input, pinned in flake.lock,
# with rust-toolchain.toml's Rust from the flake's rust-overlay, as the
# flake builds with, not the host's nixpkgs' Rust.
{ pkgs }:
let
  nodes = (builtins.fromJSON (builtins.readFile ../flake.lock)).nodes;
  fetch =
    locked:
    fetchTarball {
      url = "https://github.com/${locked.owner}/${locked.repo}/archive/${locked.rev}.tar.gz";
      sha256 = locked.narHash;
    };
  withRust = pkgs.extend (import (fetch nodes.rust-overlay.locked));
in
(import (fetch nodes.crane.locked) { pkgs = withRust; }).overrideToolchain (
  p: p.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml
)
