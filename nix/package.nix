# The site server with its static files, ready to run:
#   STATIC_DIR=$out/share/taichichuan-mobi/dist
{
  lib,
  rustPlatform,
}:
let
  fs = lib.fileset;
in
rustPlatform.buildRustPackage {
  pname = "taichichuan-mobi";
  version = (lib.importTOML ../Cargo.toml).package.version;

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

  cargoLock.lockFile = ../Cargo.lock;

  postInstall = ''
    share=$out/share/taichichuan-mobi
    mkdir -p $share
    cp -r dist $share/
  '';

  meta = {
    description = "Taiji-Timo online Tai Chi classes site";
    mainProgram = "taichichuan-mobi";
  };
}
