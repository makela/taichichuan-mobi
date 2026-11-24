# NixOS module running copies of the site as hardened systemd services, one
# per instance. Import it in the server's configuration.nix, e.g. from a clone
# of this repository, and put a reverse proxy such as nginx in front:
#
#   imports = [ /srv/taichichuan-mobi/nix/module.nix ];
#   services.taichichuan-mobi.instances = {
#     www = {
#       # The lessons and the members' pages; without this, the front page only.
#       contentDir = "/srv/taichichuan-mobi-course";
#       smtp = {
#         host = "smtp.gmail.com";
#         user = "taichiwayfarer@gmail.com";
#         passwordFile = "/var/lib/secrets/taichichuan-mobi/gmail-app-password";
#       };
#       stripe = {
#         secretKeyFile = "/var/lib/secrets/taichichuan-mobi/stripe-secret-key";
#         webhookSecretFile = "/var/lib/secrets/taichichuan-mobi/stripe-webhook-secret";
#         monthlyPrice = "price_...";
#         yearlyPrice = "price_...";
#       };
#     };
#     test = {
#       package = pkgs.callPackage /srv/taichichuan-mobi-test/nix/package.nix { };
#       port = 3101;
#       siteUrl = "https://test.taichichuan.mobi";
#     };
#   };
#
# Each instance runs as the service taichichuan-mobi-<name>, with its members'
# database in /var/lib/taichichuan-mobi-<name>. The course directory is read
# through the group taichichuan-mobi, so give it to that group:
#
#   chgrp -R taichichuan-mobi /srv/taichichuan-mobi-course
#   chmod -R g+rX,o-rwx /srv/taichichuan-mobi-course
#
# The secret files stay outside the Nix store; the service reads them through
# systemd credentials, so they can be readable by root only.
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

        siteUrl = lib.mkOption {
          type = lib.types.str;
          default = "https://www.taichichuan.mobi";
          description = "Public address of the site, used in the page's search engine and link preview tags and in links in emails.";
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

        contentDir = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          example = "/srv/taichichuan-mobi-course";
          description = ''
            Directory with the course (CONTENT_DIR), outside the Nix store and
            not under /home, readable by the group taichichuan-mobi. Set, the
            site has the lessons and the members' pages, which need smtp.host;
            null, only the front page.
          '';
        };

        mailFrom = lib.mkOption {
          type = lib.types.str;
          default = "Taiji-Timo <taichiwayfarer@gmail.com>";
          description = "Who the login emails are from (MAIL_FROM). With Gmail, the account's own address.";
        };

        smtp = {
          host = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "smtp.gmail.com";
            description = "SMTP server the login emails are sent through; needed with contentDir.";
          };
          port = lib.mkOption {
            type = lib.types.nullOr lib.types.port;
            default = null;
            description = "Port of the SMTP server, or null for 465 with tls and 587 otherwise.";
          };
          encryption = lib.mkOption {
            type = lib.types.enum [
              "tls"
              "starttls"
              "none"
            ];
            default = "tls";
            description = "tls (usually port 465, as Gmail), starttls (usually 587), or none for a local relay.";
          };
          user = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = "SMTP login, or null for a server that needs none.";
          };
          passwordFile = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "/var/lib/secrets/taichichuan-mobi/gmail-app-password";
            description = "File with the SMTP password, e.g. a Gmail app password. Needed with a user.";
          };
        };

        stripe = {
          secretKeyFile = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "/var/lib/secrets/taichichuan-mobi/stripe-secret-key";
            description = "File with the Stripe secret key (sk_live_...), or null for no payments: memberships can't then be bought.";
          };
          webhookSecretFile = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = "File with the signing secret (whsec_...) of the Stripe webhook endpoint at <siteUrl>/stripe/webhook.";
          };
          monthlyPrice = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = "ID of the membership's monthly price in Stripe (price_...).";
          };
          yearlyPrice = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = "ID of the membership's yearly price in Stripe (price_...).";
          };
          automaticTax = lib.mkOption {
            type = lib.types.bool;
            default = false;
            description = "Whether Stripe Tax adds VAT by the customer's address; needs Stripe Tax set up.";
          };
        };
      };
    };

  service =
    name: instance:
    let
      members = instance.contentDir != null;
      smtpPassword = members && instance.smtp.passwordFile != null;
      stripe = members && instance.stripe.secretKeyFile != null;
    in
    lib.nameValuePair "taichichuan-mobi-${name}" {
      description = "Taiji-Timo site (${name})";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      environment = {
        HOST = instance.host;
        PORT = toString instance.port;
        SITE_URL = instance.siteUrl;
        STATIC_DIR = "${instance.package}/share/taichichuan-mobi/dist";
        RUST_LOG = instance.logLevel;
      }
      // lib.optionalAttrs members {
        CONTENT_DIR = instance.contentDir;
        DATABASE_URL = "sqlite:///var/lib/taichichuan-mobi-${name}/taichichuan-mobi.db";
        MAIL_FROM = instance.mailFrom;
        SMTP_HOST = instance.smtp.host;
        SMTP_ENCRYPTION = instance.smtp.encryption;
      }
      // lib.optionalAttrs (members && instance.smtp.port != null) {
        SMTP_PORT = toString instance.smtp.port;
      }
      // lib.optionalAttrs (members && instance.smtp.user != null) {
        SMTP_USER = instance.smtp.user;
      }
      // lib.optionalAttrs smtpPassword {
        # %d is the service's credentials directory, see LoadCredential.
        SMTP_PASSWORD_FILE = "%d/smtp-password";
      }
      // lib.optionalAttrs stripe {
        STRIPE_SECRET_KEY_FILE = "%d/stripe-secret-key";
        STRIPE_WEBHOOK_SECRET_FILE = "%d/stripe-webhook-secret";
        STRIPE_PRICE_MONTHLY = instance.stripe.monthlyPrice;
        STRIPE_PRICE_YEARLY = instance.stripe.yearlyPrice;
        STRIPE_AUTOMATIC_TAX = if instance.stripe.automaticTax then "true" else "false";
      };

      serviceConfig = {
        ExecStart = lib.getExe instance.package;
        Restart = "on-failure";

        # The server reads from the Nix store and listens on a port; with the
        # lessons, it also reads the course, keeps the members in its state
        # directory, and sends emails and asks Stripe over the network.
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
        ]
        # Looking up the SMTP server's and Stripe's addresses, e.g. through nscd.
        ++ lib.optional members "AF_UNIX";
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
      }
      // lib.optionalAttrs members {
        StateDirectory = "taichichuan-mobi-${name}";
        SupplementaryGroups = [ "taichichuan-mobi" ];
        LoadCredential =
          lib.optional smtpPassword "smtp-password:${instance.smtp.passwordFile}"
          ++ lib.optionals stripe [
            "stripe-secret-key:${instance.stripe.secretKeyFile}"
            "stripe-webhook-secret:${instance.stripe.webhookSecretFile}"
          ];
      };
    };

  # host:port of each instance, to catch two instances on the same port.
  addresses = lib.mapAttrsToList (_: instance: "${instance.host}:${toString instance.port}") instances;

  # Each instance's settings that must go together.
  instanceAssertions =
    name: instance:
    let
      option = "services.taichichuan-mobi.instances.${name}";
      stripe = instance.stripe;
    in
    [
      {
        assertion = instance.contentDir == null || instance.smtp.host != null;
        message = "${option}: contentDir needs smtp.host, to send the login emails.";
      }
      {
        assertion = instance.smtp.user == null || instance.smtp.passwordFile != null;
        message = "${option}.smtp: a user needs a passwordFile.";
      }
      {
        assertion =
          stripe.secretKeyFile == null
          || (
            stripe.webhookSecretFile != null && stripe.monthlyPrice != null && stripe.yearlyPrice != null
          );
        message = "${option}.stripe: a secretKeyFile needs webhookSecretFile, monthlyPrice and yearlyPrice.";
      }
      {
        assertion = stripe.secretKeyFile == null || instance.contentDir != null;
        message = "${option}.stripe: payments are for the members' pages, which need contentDir.";
      }
    ];
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
    ]
    ++ lib.concatLists (lib.mapAttrsToList instanceAssertions instances);

    # Reads the course directories.
    users.groups.taichichuan-mobi = lib.mkIf (lib.any (instance: instance.contentDir != null) (
      lib.attrValues instances
    )) { };

    systemd.services = lib.mapAttrs' service instances;
  };
}
