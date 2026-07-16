{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.foundryvtt;
  addonType = kind: lib.types.attrsOf (lib.types.submodule ({name, ...}: {
    options = {
      source = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Immutable Nix source for the ${kind} ${name}; absent tombstones do not require a source.";
      };
      version = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "Human-readable upstream version recorded with this declaration.";
      };
      state = lib.mkOption {
        type = lib.types.enum ["present" "absent"];
        default = "present";
        description = "present installs the declaration; absent removes the existing entry as a tombstone.";
      };
      replace = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "For mutable worlds, replace an existing data directory during activation.";
      };
    };
  }));

  safeName = name:
    assert lib.match "[A-Za-z0-9._-]+" name != null;
    name;

  syncEntry = kind: root: name: entry: let
    id = safeName name;
    target = "${cfg.dataDir}/Data/${root}/${id}";
    source = lib.optionalString (entry.source != null) (toString entry.source);
    remove = ''rm -rf -- ${lib.escapeShellArg target}'';
    present =
      if entry.source == null
      then ''echo "${kind} ${id} is present but has no source" >&2; exit 1''
      else if kind == "world"
      then ''
        if [ -e ${lib.escapeShellArg target} ] && [ "${if entry.replace then "replace" else "preserve"}" = preserve ]; then
          : # A world is mutable state; an undeclared or non-replacing update preserves it.
        else
          rm -rf -- ${lib.escapeShellArg target}
          install -d -m 0750 ${lib.escapeShellArg target}
          cp -a --reflink=auto ${lib.escapeShellArg source}/. ${lib.escapeShellArg target}/
        fi
      ''
      else ''
        rm -rf -- ${lib.escapeShellArg target}
        ln -s -- ${lib.escapeShellArg source} ${lib.escapeShellArg target}
      '';
  in
    if entry.state == "absent" then remove else present;

  entries =
    lib.concatStringsSep "\n"
    (lib.concatLists [
      (lib.mapAttrsToList (syncEntry "module" "modules") cfg.addons.modules)
      (lib.mapAttrsToList (syncEntry "system" "systems") cfg.addons.systems)
      (lib.mapAttrsToList (syncEntry "world" "worlds") cfg.addons.worlds)
    ]);
  syncScript = pkgs.writeShellScript "foundryvtt-addon-sync" ''
    set -euo pipefail
    install -d -m 0750 "${cfg.dataDir}/Data/modules" "${cfg.dataDir}/Data/systems" "${cfg.dataDir}/Data/worlds"
    ${entries}
  '';
in {
  options.services.foundryvtt.addons = {
    modules = lib.mkOption {
      type = addonType "module";
      default = {};
    };
    systems = lib.mkOption {
      type = addonType "system";
      default = {};
    };
    worlds = lib.mkOption {
      type = addonType "world";
      default = {};
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.foundryvtt-addon-sync = {
      description = "Synchronize declarative Foundry VTT add-ons";
      before = ["foundryvtt.service"];
      wantedBy = ["multi-user.target"];
      serviceConfig = {
        Type = "oneshot";
        User = "foundryvtt";
        Group = "foundryvtt";
        ExecStart = syncScript;
        RemainAfterExit = true;
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [cfg.dataDir];
      };
    };
    systemd.services.foundryvtt = {
      requires = ["foundryvtt-addon-sync.service"];
      after = ["foundryvtt-addon-sync.service"];
    };
  };
}
