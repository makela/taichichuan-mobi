# Crane for builds outside the flake (e.g. the NixOS module imported from a
# clone): the same version as the flake's crane input, pinned in flake.lock.
{ pkgs }:
let
  locked = (builtins.fromJSON (builtins.readFile ../flake.lock)).nodes.crane.locked;
  src = fetchTarball {
    url = "https://github.com/${locked.owner}/${locked.repo}/archive/${locked.rev}.tar.gz";
    sha256 = locked.narHash;
  };
in
import src { inherit pkgs; }
