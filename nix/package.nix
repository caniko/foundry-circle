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
  safeId = assert lib.assertMsg (builtins.match "[A-Za-z0-9._-]+" id != null) "Foundry package id must be a single safe path component"; id;
  source = pkgs.fetchzip {
    pname = "foundry-${kind}-${safeId}";
    inherit url hash;
    stripRoot = false;
  };
  compatibilityJson = builtins.toJSON (
    if compatibility == null
    then {}
    else compatibility
  );
in
  pkgs.runCommand "foundry-${kind}-${safeId}-${version}" {
    nativeBuildInputs = [pkgs.jq];
    passthru.foundryPackage = {inherit kind id version manifestUrl url hash downloadUrl compatibility;};
  } ''
    set -euo pipefail
    manifest="$(find ${lib.escapeShellArg source} -type f -name ${lib.escapeShellArg manifestName} -print -quit)"
    test -n "$manifest" || { echo "${kind} archive has no ${manifestName}" >&2; exit 1; }
    test "$(jq -r '.id // empty' "$manifest")" = ${lib.escapeShellArg safeId}
    test "$(jq -r '.version // empty' "$manifest")" = ${lib.escapeShellArg version}
    download="$(jq -r '.download // empty' "$manifest")"
    test -n "$download" || { echo "${manifestName} has no download URL" >&2; exit 1; }
    test "$download" = ${lib.escapeShellArg downloadUrl} || { echo "${manifestName} download URL differs from the declared URL" >&2; exit 1; }
    jq -e --argjson expected ${lib.escapeShellArg compatibilityJson} '((.compatibility // {}) | to_entries | all(.value == ($expected[.key] // .value)))' "$manifest" >/dev/null
    root="''${manifest%/${manifestName}}"
    install -d "$out"
    cp -a "$root"/. "$out"/
    jq -n --arg kind ${lib.escapeShellArg kind} --arg id ${lib.escapeShellArg safeId} --arg version ${lib.escapeShellArg version} --arg manifestUrl ${lib.escapeShellArg manifestUrl} --arg url ${lib.escapeShellArg url} --arg hash ${lib.escapeShellArg hash} --arg downloadUrl ${lib.escapeShellArg downloadUrl} --argjson compatibility ${lib.escapeShellArg compatibilityJson} \
      '{schemaVersion: 1, kind: $kind, id: $id, version: $version, manifestUrl: $manifestUrl, url: $url, hash: $hash, downloadUrl: $downloadUrl, compatibility: $compatibility}' > "$out/.foundry-package.json"
  ''
