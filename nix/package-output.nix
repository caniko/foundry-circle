{
  lib,
  pkgs,
  source,
  kind,
  id,
  version,
  manifestName,
  manifestUrl,
  url,
  hash,
  downloadUrl,
  compatibility ? null,
}: let
  compatibilityJson = builtins.toJSON (
    if compatibility == null
    then {}
    else compatibility
  );
in
  pkgs.runCommand "foundry-${kind}-${id}-${version}" {
    nativeBuildInputs = [pkgs.jq];
    passthru.foundryPackage = {inherit kind id version manifestUrl url hash downloadUrl compatibility;};
  } ''
    set -euo pipefail
    mapfile -t manifests < <(find ${lib.escapeShellArg source} -type f -name ${lib.escapeShellArg manifestName} -print)
    test "''${#manifests[@]}" -eq 1 || { echo "${kind} archive must contain exactly one ${manifestName}; found ''${#manifests[@]}" >&2; exit 1; }
    manifest="''${manifests[0]}"
    test "$(jq -r '.id // empty' "$manifest")" = ${lib.escapeShellArg id}
    test "$(jq -r '.version // empty' "$manifest")" = ${lib.escapeShellArg version}
    download="$(jq -r '.download // empty' "$manifest")"
    test -n "$download" || { echo "${manifestName} has no download URL" >&2; exit 1; }
    test "$download" = ${lib.escapeShellArg downloadUrl} || { echo "${manifestName} download URL differs from the declared URL" >&2; exit 1; }
    jq -e --argjson expected ${lib.escapeShellArg compatibilityJson} '. as $manifest | $expected | to_entries | all(. as $item | (($manifest.compatibility // {})[$item.key] // null) == $item.value)' "$manifest" >/dev/null
    root="''${manifest%/${manifestName}}"
    install -d "$out"
    cp -a "$root"/. "$out"/
    chmod u+w "$out"
    jq -n --arg kind ${lib.escapeShellArg kind} --arg id ${lib.escapeShellArg id} --arg version ${lib.escapeShellArg version} --arg manifestUrl ${lib.escapeShellArg manifestUrl} --arg url ${lib.escapeShellArg url} --arg hash ${lib.escapeShellArg hash} --arg downloadUrl ${lib.escapeShellArg downloadUrl} --argjson compatibility ${lib.escapeShellArg compatibilityJson} \
      '{schemaVersion: 1, kind: $kind, id: $id, version: $version, manifestUrl: $manifestUrl, url: $url, hash: $hash, downloadUrl: $downloadUrl, compatibility: $compatibility}' > "$out/.foundry-package.json"
  ''
