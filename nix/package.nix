{
  lib,
  pkgs,
}: {
  kind,
  id,
  version,
  manifestUrl,
  url,
  hash,
  downloadUrl ? url,
  compatibility ? null,
}: let
  manifestName =
    if kind == "module"
    then "module.json"
    else if kind == "system"
    then "system.json"
    else throw "Foundry packages must be modules or systems";
  safeId = assert lib.assertMsg (id != "." && id != ".." && builtins.match "[A-Za-z0-9._-]+" id != null) "Foundry package id must be a single safe path component"; id;
  source = pkgs.fetchzip {
    pname = "foundry-${kind}-${safeId}";
    inherit url hash;
    stripRoot = false;
  };
in
  import ./package-output.nix {
    inherit lib pkgs source kind version manifestUrl url hash downloadUrl compatibility;
    id = safeId;
    inherit manifestName;
  }
