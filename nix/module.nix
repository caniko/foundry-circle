{
  lib,
  config,
  pkgs,
  ...
}: let
  cfg = config.services.foundry-circle;
  packageType = lib.types.nullOr lib.types.package;
in {
  imports = [
    (lib.mkRenamedOptionModule
      ["services" "foundry-circle" "foundryApiUser"]
      ["services" "foundry-circle" "foundryWorld" "apiUser"])
    (lib.mkRemovedOptionModule
      ["services" "foundry-circle" "foundryApiUserPasswordFile"]
      "Foundry Circle uses Kanidm + Rauthy for human authentication and has no password credential.")
    (lib.mkRemovedOptionModule
      ["services" "foundry-circle" "oidc" "clientSecretFile"]
      "Foundry Circle uses a public PKCE client; configure Rauthy issuer, clientId, publicBaseUrl, and scopes instead.")
  ];

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

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open `listenPort` in the firewall.";
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

    foundryWorld = {
      apiUser = lib.mkOption {
        type = lib.types.str;
        default = "foundry-circle-api";
        description = "Foundry user the broker drives the adopted world as. Adoption creates it (or reuses an existing user of this name) with a derived password; players never type it.";
      };
      baseUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Canonical Foundry origin used by the authenticated browser driver.";
      };
      passwordSecretFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Runtime HMAC secret for broker-mapped Foundry passwords (`/_circle/enter` and world adoption). Without it both answer 503.";
      };
      adminPasswordFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Runtime credential containing the Foundry admin password; the broker uses it to stop the world when a session ends. Without it the session endpoints answer 503 and the reconciler only observes.";
      };
      restartUnit = lib.mkOption {
        type = lib.types.str;
        default = "foundryvtt.service";
        description = "systemd unit restarted when a session ends; the polkit rule is scoped to this unit's restart.";
      };
      foundryVersion = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
      };
      systemId = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Expected world game system; null accepts the system chosen in Foundry's setup UI.";
      };
      systemVersion = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Expected game system version; null accepts whatever version the world runs.";
      };
      playBaseUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Public origin players join the live game from; the lobby links to `<playBaseUrl>/_circle/enter`.";
      };
      chromiumPackage = lib.mkOption {
        type = lib.types.package;
        default = pkgs.chromium;
        description = "Chromium executable used for the Foundry browser session.";
      };
    };

    oidc = {
      issuer = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Rauthy issuer URL. Kanidm is upstream of Rauthy and is not a Foundry Circle client.";
      };
      clientId = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
      };
      publicBaseUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Canonical public origin used for OIDC redirect and origin checks.";
      };
      scopes = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = ["openid" "profile" "email" "groups"];
      };
      accessGroup = lib.mkOption {
        type = lib.types.str;
        default = "foundry-circle-users";
      };
      adminGroup = lib.mkOption {
        type = lib.types.str;
        default = "foundry-circle-admins";
      };
      gmGroup = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Group whose members get the GM lobby (Foundry setup link, invite helper). Admins outrank GMs; everyone else in `accessGroup` is a player.";
      };
      sessionTtlSeconds = lib.mkOption {
        type = lib.types.ints.positive;
        default = 43200;
        description = "Maximum opaque browser-session lifetime; no refresh token is stored.";
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
        {
          assertion = cfg.oidc.issuer != null && cfg.oidc.clientId != null && cfg.oidc.publicBaseUrl != null;
          message = "services.foundry-circle.oidc must define the Rauthy issuer, public clientId, and publicBaseUrl; Foundry Circle has no local password or OIDC client-secret fallback";
        }
        {
          assertion = lib.all (value: value != null) [
            cfg.foundryWorld.baseUrl
            cfg.foundryWorld.foundryVersion
          ];
          message = "services.foundry-circle.foundryWorld must define baseUrl and foundryVersion";
        }
      ];

      networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [cfg.listenPort];

      users.users.foundry-circle = {
        isSystemUser = true;
        group = "foundry-circle";
      };
      users.groups.foundry-circle = {};

      systemd.services.foundry-circle = {
        description = "Foundry Circle typed Foundry VTT broker";
        wantedBy = ["multi-user.target"];
        after = ["network-online.target"] ++ lib.optional cfg.database.createLocally "postgresql.service";
        wants = ["network-online.target"] ++ lib.optional cfg.database.createLocally "postgresql.service";
        environment =
          {
            FOUNDRY_WORLD_API_USER = cfg.foundryWorld.apiUser;
            FOUNDRY_WORLD_BASE_URL = cfg.foundryWorld.baseUrl;
            FOUNDRY_EXPECTED_FOUNDRY_VERSION = cfg.foundryWorld.foundryVersion;
            FOUNDRY_EXPECTED_SYSTEM_ID = cfg.foundryWorld.systemId;
            FOUNDRY_EXPECTED_SYSTEM_VERSION = cfg.foundryWorld.systemVersion;
            FOUNDRY_CIRCLE_PLAY_BASE_URL = cfg.foundryWorld.playBaseUrl;
            FOUNDRY_CHROMIUM = "${cfg.foundryWorld.chromiumPackage}/bin/chromium";
            FOUNDRY_BROWSER_PROFILE_DIR = "/run/foundry-circle/browser";
            FOUNDRY_CIRCLE_RESTART_UNIT = cfg.foundryWorld.restartUnit;
            FOUNDRY_CIRCLE_SYSTEMCTL = "${config.systemd.package}/bin/systemctl";
            XDG_CONFIG_HOME = "/var/lib/foundry-circle/config";
            FOUNDRY_CIRCLE_BIND = "${cfg.listenAddress}:${toString cfg.listenPort}";
            RUST_LOG = lib.mkDefault "info";
            DIOXUS_PUBLIC_PATH = "${cfg.package}/${cfg.package.dioxus.publicDir or "share/foundry-circle/public"}";
          }
          // lib.optionalAttrs (cfg.database.url != null || cfg.database.createLocally) {
            DATABASE_URL =
              if cfg.database.url != null
              then cfg.database.url
              else "postgresql:///${cfg.database.name}?host=/run/postgresql&user=${cfg.database.user}";
          }
          // lib.optionalAttrs (cfg.database.urlFile != null) {
            DATABASE_URL_FILE = "/run/credentials/foundry-circle.service/database-url";
          }
          // lib.optionalAttrs (cfg.foundryWorld.adminPasswordFile != null) {
            FOUNDRY_ADMIN_PASSWORD_FILE = "/run/credentials/foundry-circle.service/admin-password";
          }
          // lib.optionalAttrs (cfg.foundryWorld.passwordSecretFile != null) {
            FOUNDRY_CIRCLE_PASSWORD_SECRET_FILE = "/run/credentials/foundry-circle.service/password-secret";
          }
          // lib.optionalAttrs (cfg.oidc.issuer != null && cfg.oidc.clientId != null && cfg.oidc.publicBaseUrl != null) {
            FOUNDRY_CIRCLE_OIDC_ISSUER = cfg.oidc.issuer;
            FOUNDRY_CIRCLE_OIDC_CLIENT_ID = cfg.oidc.clientId;
            FOUNDRY_CIRCLE_OIDC_PUBLIC_BASE_URL = cfg.oidc.publicBaseUrl;
            FOUNDRY_CIRCLE_OIDC_SCOPES = lib.concatStringsSep " " cfg.oidc.scopes;
            FOUNDRY_CIRCLE_OIDC_ACCESS_GROUP = cfg.oidc.accessGroup;
            FOUNDRY_CIRCLE_OIDC_ADMIN_GROUP = cfg.oidc.adminGroup;
            FOUNDRY_CIRCLE_OIDC_GM_GROUP = cfg.oidc.gmGroup;
            FOUNDRY_CIRCLE_SESSION_TTL_SECONDS = toString cfg.oidc.sessionTtlSeconds;
          };
        serviceConfig = {
          User = "foundry-circle";
          Group = "foundry-circle";
          ExecStart = "${cfg.package}/bin/foundry-circle";
          RuntimeDirectory = "foundry-circle";
          StateDirectory = "foundry-circle";
          Restart = "on-failure";
          RestartSec = "30s";
          NoNewPrivileges = true;
          PrivateTmp = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          ReadWritePaths = ["/run/foundry-circle" "/var/lib/foundry-circle"];
          LoadCredential =
            lib.optional (cfg.foundryWorld.passwordSecretFile != null)
            "password-secret:${cfg.foundryWorld.passwordSecretFile}"
            ++ lib.optional (cfg.foundryWorld.adminPasswordFile != null)
            "admin-password:${cfg.foundryWorld.adminPasswordFile}"
            ++ lib.optional (cfg.database.urlFile != null)
            "database-url:${cfg.database.urlFile}";
        };
        unitConfig = {
          StartLimitIntervalSec = "5min";
          StartLimitBurst = 5;
        };
      };
    }
    (lib.mkIf cfg.database.createLocally {
      services.postgresql = {
        ensureDatabases = [cfg.database.name];
        ensureUsers = [
          {
            name = cfg.database.user;
          }
        ];
      };
      systemd.services.postgresql-setup.script = lib.mkAfter ''
        psql -tAc 'ALTER DATABASE "${cfg.database.name}" OWNER TO "${cfg.database.user}";'
      '';
    })
    # Ending a session restarts Foundry to drop every in-memory session. The
    # service user may restart exactly that one unit and nothing else.
    (lib.mkIf (cfg.foundryWorld.adminPasswordFile != null) {
      security.polkit.extraConfig = ''
        polkit.addRule(function(action, subject) {
          if (action.id == "org.freedesktop.systemd1.manage-units"
              && subject.user == "foundry-circle"
              && action.lookup("unit") == "${cfg.foundryWorld.restartUnit}"
              && action.lookup("verb") == "restart") {
            return polkit.Result.YES;
          }
        });
      '';
    })
  ]);
}
