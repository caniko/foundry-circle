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
    };

    foundryApiUserPasswordFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Runtime-only password file for the dedicated Foundry API user.";
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
            lib.optional (cfg.foundryApiUserPasswordFile != null)
            "foundry-api-user-password:${cfg.foundryApiUserPasswordFile}";
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
