# NixOS module running copies of the site as hardened systemd services, one
# per instance. Import it in the server's configuration.nix, e.g. from a clone
# of this repository, and put a reverse proxy such as nginx in front:
#
#   imports = [ /srv/taichichuan-mobi/nix/module.nix ];
#   services.taichichuan-mobi.instances = {
#     www = { };
#     test = {
#       package = pkgs.callPackage /srv/taichichuan-mobi-test/nix/package.nix { };
#       port = 3101;
#     };
#   };
#
# Each instance runs as the service taichichuan-mobi-<name>.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  instances = lib.filterAttrs (_: instance: instance.enable) config.services.taichichuan-mobi.instances;

  instanceOptions =
    { name, ... }:
    {
      options = {
        enable = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Whether to run this instance.";
        };

        package = lib.mkOption {
          type = lib.types.package;
          default = pkgs.callPackage ./package.nix { };
          defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
          description = "The site package, including its static files.";
        };

        host = lib.mkOption {
          type = lib.types.str;
          default = "127.0.0.1";
          description = "Address to listen on. The default only accepts local connections, e.g. from a reverse proxy.";
        };

        port = lib.mkOption {
          type = lib.types.port;
          default = 3100;
          description = "Port to listen on (instance ${name}).";
        };

        logLevel = lib.mkOption {
          type = lib.types.str;
          default = "taichichuan_mobi=info,tower_http=info";
          description = "Log filter (RUST_LOG). Logs go to the journal.";
        };
      };
    };

  service =
    name: instance:
    lib.nameValuePair "taichichuan-mobi-${name}" {
      description = "Taiji-Timo site (${name})";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      environment = {
        HOST = instance.host;
        PORT = toString instance.port;
        STATIC_DIR = "${instance.package}/share/taichichuan-mobi/dist";
        RUST_LOG = instance.logLevel;
      };

      serviceConfig = {
        ExecStart = lib.getExe instance.package;
        Restart = "on-failure";

        # The server only reads from the Nix store and listens on a port.
        DynamicUser = true;
        WorkingDirectory = "/";
        CapabilityBoundingSet = "";
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectControlGroups = true;
        ProtectProc = "invisible";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
        ];
        UMask = "0077";
      };
    };

  # host:port of each instance, to catch two instances on the same port.
  addresses = lib.mapAttrsToList (_: instance: "${instance.host}:${toString instance.port}") instances;
in
{
  options.services.taichichuan-mobi.instances = lib.mkOption {
    type = lib.types.attrsOf (lib.types.submodule instanceOptions);
    default = { };
    description = "Copies of the site to run, by name.";
  };

  config = lib.mkIf (instances != { }) {
    assertions = [
      {
        assertion = lib.length addresses == lib.length (lib.unique addresses);
        message = "services.taichichuan-mobi.instances: each instance needs its own port (${lib.concatStringsSep ", " addresses}).";
      }
    ];

    systemd.services = lib.mapAttrs' service instances;
  };
}
