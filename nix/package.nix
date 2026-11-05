# The site server with its static files, ready to run:
#   STATIC_DIR=$out/share/taichichuan-mobi/dist
#
# Crane builds the cargo dependencies in their own derivation, so changing the
# blog's code, posts or assets only rebuilds the blog itself.
{
  lib,
  pkgs,
  # The flake passes its crane input; elsewhere use the version in flake.lock.
  craneLib ? import ./crane.nix { inherit pkgs; },
}:
let
  fs = lib.fileset;
  commonArgs = {
    pname = "taichichuan-mobi";
    version = (lib.importTOML ../Cargo.toml).package.version;
    inherit src;
    strictDeps = true;
  };

  # Only what the build needs, so e.g. target/ or README edits don't
  # cause rebuilds when this is used straight from a clone.
  src = fs.toSource {
    root = ../.;
    fileset = fs.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../dist
    ];
  };

  # Release dependencies only; rebuilt when Cargo.toml or Cargo.lock change.
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;

  postInstall = ''
    share=$out/share/taichichuan-mobi
    mkdir -p $share
    cp -r dist $share/
  '';

  meta = {
    description = "Taiji-Timo online Tai Chi classes site";
    mainProgram = "taichichuan-mobi";
  };
})
