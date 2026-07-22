{
  config,
  lib,
  nixFoundryvtt,
  pkgs,
  ...
}: let
  cfg = config.services.foundryvtt.acquisition;
  registry = lib.importJSON "${nixFoundryvtt}/pkgs/foundryvtt/versions.json";
  registryEntry =
    if builtins.hasAttr cfg.version registry
    then registry.${cfg.version}
    else throw "Foundry Circle: ${cfg.version} is not present in the pinned nix-foundryvtt versions.json";
  licensedSource = pkgs.stdenvNoCC.mkDerivation {
    pname = "foundryvtt-licensed-source";
    version = cfg.version;
    name = "FoundryVTT-Linux-${cfg.version}.zip";
    outputHashMode = "flat";
    outputHash = registryEntry.hash;
    allowSubstitutes = false;
    preferLocalBuild = true;
    requiredSystemFeatures = ["foundry-license"];
    FOUNDRY_LICENSED_SOURCE = "1";
    FOUNDRY_RELEASE = cfg.version;
    FOUNDRY_PLATFORM = "linux";
    FOUNDRY_EXPECTED_SRI = registryEntry.hash;
    buildCommand = ''
      test -s /build/foundryvtt-licensed-source || {
        echo "Foundry Circle: acquisition hook did not provide the licensed archive" >&2
        exit 1
      }
      install -m 0440 /build/foundryvtt-licensed-source "$out"
    '';
  };
  upstreamPackage = pkgs.callPackage "${nixFoundryvtt}/pkgs/foundryvtt" {
    requireFile = _: licensedSource;
  };
  licensedPackage = upstreamPackage.overrideAttrs (_: {
    version = cfg.version;
  });
  hook = "${cfg.fetchPackage}/bin/foundryvtt-fetch-hook";
  daemon = "${cfg.fetchPackage}/bin/foundryvtt-fetchd";
  # Nix executes `pre-build-hook` as a single executable path; it does not
  # perform shell-style argument splitting. Keep the configured value free of
  # arguments and forward the hook's derivation arguments from a wrapper.
  hookWrapper = pkgs.writeShellScript "foundryvtt-fetch-hook-wrapper" ''
    exec ${lib.escapeShellArg hook} \
      --socket ${lib.escapeShellArg cfg.socketPath} \
      --nix ${lib.escapeShellArg "${pkgs.nix}/bin/nix"} \
      "$@"
  '';
in {
  options.services.foundryvtt.acquisition = {
    enable = lib.mkEnableOption "declarative licensed Foundry VTT acquisition";
    version = lib.mkOption {
      type = lib.types.strMatching "[0-9]+\\.[0-9]+";
      default = "13.351";
      description = "Exact major.build release from the pinned nix-foundryvtt registry.";
    };
    releaseType = lib.mkOption {
      type = lib.types.enum ["prototype" "development" "testing" "stable"];
      default = "stable";
      description = "Release channel asserted against nix-foundryvtt versions.json.";
    };
    accountUsername = lib.mkOption {
      type = lib.types.str;
      description = "Foundry account username used only by the credential-bearing daemon.";
    };
    accountPasswordFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Agenix-rendered Foundry account password loaded by systemd, never put in the store.";
    };
    cacheDir = lib.mkOption {
      type = lib.types.path;
      default = "/var/lib/foundryvtt-acquisition";
      description = "Private, local-only cache shared read-only with Nix build users.";
    };
    socketPath = lib.mkOption {
      type = lib.types.path;
      default = "/run/foundryvtt-acquisition/socket";
      description = "Unix socket used by the Nix pre-build hook.";
    };
    site = lib.mkOption {
      type = lib.types.str;
      default = "https://foundryvtt.com";
      description = "Official Foundry origin used by the account acquisition flow.";
    };
    fetchPackage = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Foundry Circle package containing foundryvtt-fetchd and foundryvtt-fetch-hook.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.fetchPackage != null;
        message = "services.foundryvtt.acquisition.fetchPackage must be set when acquisition is enabled";
      }
      {
        assertion = cfg.accountPasswordFile != null;
        message = "services.foundryvtt.acquisition.accountPasswordFile must reference the existing agenix account secret";
      }
      {
        assertion = registryEntry.releaseType == cfg.releaseType;
        message = "Foundry Circle: ${cfg.version} is ${registryEntry.releaseType} in nix-foundryvtt, not ${cfg.releaseType}";
      }
    ];

    services.foundryvtt.package = lib.mkDefault licensedPackage;
    nix.settings = {
      pre-build-hook = hookWrapper;
      system-features = lib.mkAfter ["foundry-license"];
    };
    users.users.foundryvtt-acquire = {
      isSystemUser = true;
      group = "foundryvtt-acquire";
      extraGroups = ["nixbld"];
    };
    users.groups.foundryvtt-acquire = {};
    systemd.tmpfiles.settings."10-foundryvtt-acquisition" = {
      ${cfg.cacheDir}.d = {
        mode = "2750";
        user = "foundryvtt-acquire";
        group = "nixbld";
      };
    };
    systemd.services.foundryvtt-acquisition = {
      description = "Credential-bearing Foundry VTT archive acquisition";
      wantedBy = ["multi-user.target"];
      after = ["network-online.target"];
      wants = ["network-online.target"];
      serviceConfig = {
        User = "foundryvtt-acquire";
        Group = "foundryvtt-acquire";
        SupplementaryGroups = ["nixbld"];
        ExecStartPre = [
          "+${pkgs.coreutils}/bin/chown -R foundryvtt-acquire:nixbld ${cfg.cacheDir}"
        ];
        ExecStart = "${daemon} --socket ${cfg.socketPath} --cache-dir ${cfg.cacheDir} --username ${lib.escapeShellArg cfg.accountUsername} --site ${lib.escapeShellArg cfg.site}";
        ExecStartPost = "+${pkgs.coreutils}/bin/chgrp nixbld ${cfg.socketPath}";
        RuntimeDirectory = "foundryvtt-acquisition";
        # Nix build users reach the socket after ExecStartPost changes its
        # group to nixbld; they also need directory traversal to get there.
        RuntimeDirectoryMode = "0755";
        Restart = "on-failure";
        RestartSec = 5;
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [cfg.cacheDir "/run/foundryvtt-acquisition"];
        LoadCredential = ["account-password:${cfg.accountPasswordFile}"];
      };
    };
  };
}
