{
  nixpkgs,
  pkgs,
}: let
  module = import ../module.nix;
  package = pkgs.runCommand "foundry-circle-module-package" {
    passthru.dioxus.publicDir = "share/foundry-circle/public";
  } "mkdir -p $out/bin $out/share/foundry-circle/public";
  system = modules:
    nixpkgs.lib.nixosSystem {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [module] ++ modules;
    };
  enabled = system [
    (_: {
      system.stateVersion = "25.11";
      services.foundry-circle = {
        enable = true;
        inherit package;
        listenAddress = "0.0.0.0";
        listenPort = 8032;
        openFirewall = true;
        database.createLocally = true;
        assets.enable = true;
        foundryWorld = {
          baseUrl = "https://vtt.example";
          passwordSecretFile = "/run/agenix/foundry-circle-password-secret";
          adminPasswordFile = "/run/agenix/foundry-admin-password";
          foundryVersion = "14.367";
          systemId = "daggerheart";
          systemVersion = "1.6.4";
        };
        oidc = {
          issuer = "https://identity.example/auth/v1/";
          clientId = "foundry-circle";
          publicBaseUrl = "https://vtt.example";
        };
      };
    })
  ];
  disabled = system [
    (_: {
      system.stateVersion = "25.11";
    })
  ];
in
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_CIRCLE_BIND == "0.0.0.0:8032";
  assert enabled.config.systemd.services.foundry-circle.environment.DIOXUS_PUBLIC_PATH == "${package}/share/foundry-circle/public";
  assert enabled.config.systemd.services.foundry-circle.environment.XDG_CONFIG_HOME == "/var/lib/foundry-circle/config";
  assert enabled.config.systemd.services.foundry-circle.serviceConfig.ExecStart == "${package}/bin/foundry-circle";
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_WORLD_API_USER == "foundry-circle-api";
  assert !(enabled.config.systemd.services.foundry-circle.environment ? FOUNDRY_WORLD_ID);
  assert builtins.elem "password-secret:/run/agenix/foundry-circle-password-secret" enabled.config.systemd.services.foundry-circle.serviceConfig.LoadCredential;
  assert builtins.elem "admin-password:/run/agenix/foundry-admin-password" enabled.config.systemd.services.foundry-circle.serviceConfig.LoadCredential;
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_CIRCLE_PASSWORD_SECRET_FILE == "/run/credentials/foundry-circle.service/password-secret";
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_ADMIN_PASSWORD_FILE == "/run/credentials/foundry-circle.service/admin-password";
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_CIRCLE_RESTART_UNIT == "foundryvtt.service";
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_CIRCLE_SYSTEMCTL == "${enabled.config.systemd.package}/bin/systemctl";
  assert pkgs.lib.hasInfix "org.freedesktop.systemd1.manage-units" enabled.config.security.polkit.extraConfig;
  assert pkgs.lib.hasInfix "action.lookup(\"unit\") == \"foundryvtt.service\"" enabled.config.security.polkit.extraConfig;
  assert pkgs.lib.hasInfix "action.lookup(\"verb\") == \"restart\"" enabled.config.security.polkit.extraConfig;
  assert !(pkgs.lib.hasInfix "org.freedesktop.systemd1.manage-units" disabled.config.security.polkit.extraConfig);
  assert builtins.elem "foundry_circle" enabled.config.services.postgresql.ensureDatabases;
  assert pkgs.lib.any (user: user.name == "foundry-circle") enabled.config.services.postgresql.ensureUsers;
  assert builtins.elem 8032 enabled.config.networking.firewall.allowedTCPPorts;
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_CIRCLE_ASSETS_DIR == "/var/lib/foundry-circle/assets";
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_CIRCLE_ASSETS_PREFIX == "circle";
  assert builtins.elem "/var/lib/foundry-circle/assets" enabled.config.systemd.services.foundry-circle.serviceConfig.ReadWritePaths;
  assert builtins.elem "d /var/lib/foundry-circle/assets 0755 foundry-circle foundry-circle" enabled.config.systemd.tmpfiles.rules;
  assert !(builtins.hasAttr "foundry-circle" disabled.config.systemd.services);
    pkgs.runCommand "foundry-circle-module-shape" {} "touch $out"
