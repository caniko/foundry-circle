{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.foundryvtt;
  packageEntry = lib.types.submodule ({name, ...}: {
    options = {
      package = lib.mkOption {
        type = lib.types.nullOr lib.types.package;
        default = null;
        description = "Immutable output of foundry-circle.lib.mkFoundryPackage for ${name}; absent tombstones do not require a package.";
      };
      kind = lib.mkOption {
        type = lib.types.enum ["module" "system"];
        default = "module";
      };
      id = lib.mkOption {
        type = lib.types.str;
        default = name;
      };
      state = lib.mkOption {
        type = lib.types.enum ["present" "absent"];
        default = "present";
        description = "present installs or versions the package; absent removes the recorded link.";
      };
      version = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "Expected manifest version; defaults to package provenance.";
      };
    };
  });
  packageValue = name: entry: let
    provenance =
      if entry.package == null
      then {}
      else entry.package.foundryPackage or {};
    version =
      if entry.version != ""
      then entry.version
      else provenance.version or "";
    id =
      if entry.id != ""
      then entry.id
      else provenance.id or name;
  in {
    inherit (entry) kind state;
    inherit id version;
    storePath =
      if entry.package == null
      then ""
      else toString entry.package;
  };
  desired = lib.mapAttrsToList packageValue cfg.declarativePackages;
  safePackage = entry:
    entry.id
    != "."
    && entry.id != ".."
    && builtins.match "[A-Za-z0-9._-]+" entry.id != null;
  provenanceMatches = entry:
    if entry.state == "absent"
    then true
    else let
      provenance =
        if entry.package == null
        then {}
        else entry.package.foundryPackage or {};
      expectedVersion =
        if entry.version != ""
        then entry.version
        else provenance.version or "";
    in
      entry.package
      != null
      && (provenance.kind or null) == entry.kind
      && (provenance.id or null) == entry.id
      && (provenance.version or null) == expectedVersion
      && expectedVersion != "";
  desiredFile = pkgs.writeText "foundry-circle-packages.json" (builtins.toJSON {
    schemaVersion = 1;
    packages = desired;
  });
in {
  options.services.foundryvtt = {
    packageManager = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "foundryvtt-fetch package used for atomic declarative package reconciliation.";
    };
    declarativePackages = lib.mkOption {
      type = lib.types.attrsOf packageEntry;
      default = {};
      description = "Immutable Foundry modules and systems managed as Nix package outputs.";
    };
  };
  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      assertions = [
        {
          assertion = cfg.declarativePackages == {} || cfg.packageManager != null;
          message = "services.foundryvtt.packageManager must be set when declarativePackages are configured";
        }
        {
          assertion = lib.all safePackage (lib.attrValues cfg.declarativePackages);
          message = "declarative Foundry package ids must be safe path components";
        }
        {
          assertion = lib.all provenanceMatches (lib.attrValues cfg.declarativePackages);
          message = "present declarative Foundry packages must match their package kind/id/version provenance";
        }
      ];
    }
    (lib.mkIf (cfg.declarativePackages != {}) {
      systemd.services.foundryvtt.serviceConfig.ExecStartPre = lib.mkBefore [
        "${cfg.packageManager}/bin/foundryvtt-fetch reconcile --data-dir ${cfg.dataDir} --desired ${desiredFile} --state-file ${cfg.dataDir}/.foundry-circle-packages.json"
      ];
    })
  ]);
}
