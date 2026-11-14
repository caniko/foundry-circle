# Dioxus 0.8 toolchain pins.
#
# harbor-rs defaults `dioxusCli` to `pkgs.dioxus-cli`, which nixpkgs still ships
# at 0.7.10; that `dx` cannot build the 0.8 crates in Cargo.lock. nixpkgs also
# has no wasm-bindgen CLI for the 0.2.130 that dioxus-web requires, and the CLI
# and the generated bindings are an ABI pair, so both are built from their
# crates.io sources here and passed to `mkDioxusFullstackPackage`.
{pkgs}: let
  dioxusVersion = "0.8.0-alpha.3";
  wasmBindgenVersion = "0.2.130";

  wasmBindgenSrc = pkgs.fetchCrate {
    pname = "wasm-bindgen-cli";
    version = wasmBindgenVersion;
    hash = "sha256-9Og1O/6/VC3sc5hBjoIWBErmLW2UHGMsTQO+6+m/dng=";
  };

  dioxusCli = pkgs.rustPlatform.buildRustPackage {
    pname = "dioxus-cli";
    version = dioxusVersion;
    src = pkgs.fetchCrate {
      pname = "dioxus-cli";
      version = dioxusVersion;
      hash = "sha256-n1Lk9eCjr1vGqvCDbrOmbU7TTvdcGdx/chRsj4Z3a+Q=";
    };
    cargoHash = "sha256-nqGrW0Sf7DWdrSXSKcSl+FlO+1NY4uZA7G7G4AGItuw=";
    # No downloads from the sandbox: wasm-bindgen and esbuild come from PATH.
    buildFeatures = ["no-downloads" "disable-telemetry"];
    env.OPENSSL_NO_VENDOR = 1;
    nativeBuildInputs = [
      pkgs.pkg-config
      pkgs.cacert
      pkgs.makeWrapper
    ];
    buildInputs = [pkgs.openssl];
    # Upstream's tests need the wasm-bindgen monorepo and network access.
    doCheck = false;
    postFixup = ''
      wrapProgram $out/bin/dx \
        --suffix PATH : ${pkgs.lib.makeBinPath [
        pkgs.esbuild
        wasmBindgenCli
      ]}
    '';
  };

  wasmBindgenCli = pkgs.buildWasmBindgenCli {
    src = wasmBindgenSrc;
    cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
      pname = "wasm-bindgen-cli";
      version = wasmBindgenVersion;
      src = wasmBindgenSrc;
      hash = "sha256-j81sUf1m5yXU4FJfGj7GV+hdqVCV8mRmNb0QIsMCjqw=";
    };
  };
in {
  inherit dioxusCli wasmBindgenCli;
}
