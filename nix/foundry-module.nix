{
  lib,
  pkgs,
  version,
}: let
  # A module built from this checkout rather than a remote archive: the plan's
  # `mkFoundryPackage` path needs a downloaded zip with a hash and a `download`
  # field, which a repository-local module does not have.
  module = ../foundry-module;
in
  pkgs.runCommand "foundry-module-foundry-circle-${version}" {
    nativeBuildInputs = [pkgs.jq];
    # `nix/addons.nix` reads this to serialise a declarative package entry, and
    # asserts that the manifest version equals it.
    passthru.foundryPackage = {
      kind = "module";
      id = "foundry-circle";
      inherit version;
    };
  } ''
    set -euo pipefail
    install -d "$out"
    # Foundry rejects a directory whose name differs from the manifest id and
    # loads `Data/modules/<id>`, so the output root is the module itself.
    cp -a ${module}/. "$out"/
    chmod -R u+w "$out"
    test "$(jq -r '.id // empty' "$out/module.json")" = "foundry-circle"
    test "$(jq -r '.version // empty' "$out/module.json")" = ${lib.escapeShellArg version}
    jq -e '.esmodules | index("foundry-circle.mjs")' "$out/module.json" >/dev/null
    jq -n --arg version ${lib.escapeShellArg version} \
      '{schemaVersion: 1, kind: "module", id: "foundry-circle", version: $version}' > "$out/.foundry-package.json"
  ''
