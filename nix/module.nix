{
  lib,
  config,
  pkgs,
  ...
}: let
  cfg = config.services.foundry-circle;
  packageType = lib.types.nullOr lib.types.package;
in {
  options.services.foundry-circle = {
    enable = lib.mkEnableOption "the Foundry Circle broker";

    package = lib.mkOption {
      type = packageType;
      default = null;
      description = "Foundry Circle package containing the server binary and Dioxus assets.";
    };

    listenAddress = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
    };

    listenPort = lib.mkOption {
      type = lib.types.port;
      default = 8032;
    };

    database = {
      createLocally = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Create the PostgreSQL database and peer role on this host.";
      };
      name = lib.mkOption {
        type = lib.types.str;
        default = "foundry_circle";
      };
      user = lib.mkOption {
        type = lib.types.str;
        default = "foundry-circle";
      };
      url = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "PostgreSQL URL when it is safe to keep it in the Nix configuration.";
      };
      urlFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Runtime-only PostgreSQL URL credential; preferred for password-authenticated databases.";
      };
    };

    foundryApiUserPasswordFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Runtime-only password file for the dedicated Foundry API user.";
    };

    foundryApiUser = lib.mkOption {
      type = lib.types.str;
      default = "foundry-circle-api";
      description = "Dedicated Foundry API user name, provisioned in the live world.";
    };

    oidc = {
      issuer = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Rauthy issuer URL. Kanidm remains the identity source; this service trusts Rauthy only.";
      };
      clientId = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
      };
      clientSecretFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Runtime-only OIDC client secret file.";
      };
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [
        {
          assertion = cfg.package != null;
          message = "services.foundry-circle.package must be set when the service is enabled";
        }
      ];

      users.users.foundry-circle = {
        isSystemUser = true;
        group = "foundry-circle";
      };
      users.groups.foundry-circle = {};

      systemd.services.foundry-circle = {
        description = "Foundry Circle typed Foundry VTT broker";
        wantedBy = ["multi-user.target"];
        after = ["network.target"] ++ lib.optional cfg.database.createLocally "postgresql.service";
        wants = lib.optional cfg.database.createLocally "postgresql.service";
        environment = {
          FOUNDRY_API_USER = cfg.foundryApiUser;
        } // lib.optionalAttrs (cfg.database.url != null || cfg.database.createLocally) {
          DATABASE_URL =
            if cfg.database.url != null
            then cfg.database.url
            else "postgresql://${cfg.database.user}@/${cfg.database.name}?host=/run/postgresql";
        } // lib.optionalAttrs (cfg.database.urlFile != null) {
          DATABASE_URL_FILE = "/run/credentials/foundry-circle.service/database-url";
        } // lib.optionalAttrs (cfg.foundryApiUserPasswordFile != null) {
          FOUNDRY_API_USER_PASSWORD_FILE = "/run/credentials/foundry-circle.service/foundry-api-user-password";
        } // lib.optionalAttrs (cfg.oidc.issuer != null) {
          FOUNDRY_CIRCLE_OIDC_ISSUER = cfg.oidc.issuer;
        } // lib.optionalAttrs (cfg.oidc.clientId != null) {
          FOUNDRY_CIRCLE_OIDC_CLIENT_ID = cfg.oidc.clientId;
        } // lib.optionalAttrs (cfg.oidc.clientSecretFile != null) {
          FOUNDRY_CIRCLE_OIDC_CLIENT_SECRET_FILE = "/run/credentials/foundry-circle.service/oidc-client-secret";
        };
        serviceConfig = {
          User = "foundry-circle";
          Group = "foundry-circle";
          ExecStart = "${cfg.package}/bin/foundry-circle";
          Environment = "FOUNDRY_CIRCLE_BIND=${cfg.listenAddress}:${toString cfg.listenPort}";
          RuntimeDirectory = "foundry-circle";
          StateDirectory = "foundry-circle";
          Restart = "on-failure";
          NoNewPrivileges = true;
          PrivateTmp = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          ReadWritePaths = ["/run/foundry-circle" "/var/lib/foundry-circle"];
          LoadCredential =
            (lib.optional (cfg.foundryApiUserPasswordFile != null)
              "foundry-api-user-password:${cfg.foundryApiUserPasswordFile}")
            ++ (lib.optional (cfg.database.urlFile != null)
              "database-url:${cfg.database.urlFile}")
            ++ (lib.optional (cfg.oidc.clientSecretFile != null)
              "oidc-client-secret:${cfg.oidc.clientSecretFile}");
        };
      };
    }
    (lib.mkIf cfg.database.createLocally {
      services.postgresql = {
        ensureDatabases = [cfg.database.name];
        ensureUsers = [
          {
            name = cfg.database.user;
            ensureDBOwnership = true;
          }
        ];
      };
    })
  ]);
}
