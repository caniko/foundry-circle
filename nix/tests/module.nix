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
    ({...}: {
      system.stateVersion = "25.11";
      services.foundry-circle = {
        enable = true;
        inherit package;
        listenAddress = "0.0.0.0";
        listenPort = 8032;
        database.createLocally = true;
        oidc = {
          issuer = "https://identity.example/auth/v1/";
          clientId = "foundry-circle";
          publicBaseUrl = "https://vtt.example";
        };
      };
    })
  ];
  disabled = system [
    ({...}: {
      system.stateVersion = "25.11";
    })
  ];
in
  assert enabled.config.systemd.services.foundry-circle.environment.FOUNDRY_CIRCLE_BIND == "0.0.0.0:8032";
  assert enabled.config.systemd.services.foundry-circle.environment.DIOXUS_PUBLIC_PATH == "${package}/share/foundry-circle/public";
  assert enabled.config.systemd.services.foundry-circle.serviceConfig.ExecStart == "${package}/bin/foundry-circle";
  assert builtins.elem "foundry_circle" enabled.config.services.postgresql.ensureDatabases;
  assert pkgs.lib.any (user: user.name == "foundry-circle") enabled.config.services.postgresql.ensureUsers;
  assert !(builtins.hasAttr "foundry-circle" disabled.config.systemd.services);
    pkgs.runCommand "foundry-circle-module-shape" {} "touch $out"
