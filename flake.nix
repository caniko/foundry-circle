{
  description = "Foundry Circle: typed Foundry VTT broker and Dioxus operator console";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rs-harbor = {
      url = "github:caniko/rs-harbor/0.1.0";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-overlay.follows = "rust-overlay";
    };
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.follows = "rs-harbor/crane";
    flake-utils.url = "github:numtide/flake-utils";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    git-hooks.url = "github:cachix/git-hooks.nix";

    # Consumed by the service-stack module in a later phase. Keeping these as
    # first-class inputs makes the provenance and package boundaries explicit.
    nix-foundryvtt.url = "github:nix-foundryvtt/nix-foundryvtt";
    nix-provenance.url = "git+https://codeberg.org/caniko/nix-provenance.git?rev=5ac4a08dc82b53e09c7c0f62c700316e74a48219";
  };

  outputs = inputs @ {
    self,
    nixpkgs,
    rs-harbor,
    rust-overlay,
    flake-utils,
    treefmt-nix,
    git-hooks,
    nix-foundryvtt,
    ...
  }:
    (flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
      toolchain = rs-harbor.lib.mkToolchain {
        inherit pkgs;
        channel = "stable";
        extensions = ["rustfmt" "clippy" "rust-src"];
        cache.enable = false;
      };
      src = (toolchain.craneLib).cleanCargoSource ./.;
      commonArgs = {
        inherit src;
        pname = "foundry-circle";
        strictDeps = true;
      };
      foundryVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      cargoArtifacts = toolchain.craneLib.buildDepsOnly commonArgs;
      cargoPackage = toolchain.craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--workspace --all-features";
        });
      fetchPackage = toolchain.craneLib.buildPackage (commonArgs
        // {
          pname = "foundryvtt-fetch";
          cargoExtraArgs = "-p foundryvtt-fetch --bin foundryvtt-fetch";
        });
      portableRelease = rs-harbor.lib.mkPortableBinaryRelease {
        inherit pkgs;
        pname = "foundry-circle";
        version = foundryVersion;
        artifacts.x86_64-linux.entries = {
          foundry-circle.package = dioxusPackage;
          foundryvtt-fetch.package = fetchPackage;
        };
      };
      releaseBundle = portableRelease.releaseBundle;
      dioxusPackage = rs-harbor.lib.mkDioxusFullstackPackage {
        inherit pkgs src;
        craneLib = toolchain.craneLib;
        rustToolchain = toolchain.rustToolchain;
        cargoLock = ./Cargo.lock;
        pname = "foundry-circle";
        package = "foundry-circle";
        binary = "foundry-circle";
        version = foundryVersion;
        serverInstallName = "foundry-circle";
        serverBinary = "foundry-circle";
        profile = "release";
        debugSymbols = false;
        noDefaultFeatures = true;
        webFeatures = ["web"];
        serverFeatures = ["server"];
        publicSubdir = "share/foundry-circle/public";
        wrapServer = false;
      };
      treefmtEval = treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix);
      preCommit = git-hooks.lib.${system}.run {
        src = ./.;
        hooks = import ./nix/pre-commit.nix {
          inherit pkgs;
          treefmtWrapper = treefmtEval.config.build.wrapper;
          rustToolchain = toolchain.rustToolchain;
        };
      };
      devShell = toolchain.craneLib.devShell {
        checks = self.checks.${system};
        packages = with pkgs;
          [cargo-deny cargo-nextest cargo-audit jq pre-commit rust-analyzer]
          ++ preCommit.enabledPackages;
        shellHook = preCommit.shellHook;
      };
    in {
      packages = {
        default = dioxusPackage;
        foundry-circle = dioxusPackage;
        foundry-circle-cli = cargoPackage;
        foundryvtt-fetch = fetchPackage;
        release-bundle = releaseBundle;
      };

      apps.default = flake-utils.lib.mkApp {
        drv = dioxusPackage;
        exePath = "/bin/foundry-circle";
      };

      formatter = treefmtEval.config.build.wrapper;

      checks = {
        default = cargoPackage;
        formatting = treefmtEval.config.build.check self;
        fmt = toolchain.craneLib.cargoFmt {
          inherit src;
          pname = "foundry-circle";
        };
        clippy = toolchain.craneLib.cargoClippy (commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--workspace --all-targets --all-features -- --deny warnings";
          });
      };

      devShells = {
        default = devShell;
        # Simit's generated publish workflow uses this named shell for the
        # crate documentation gate.
        docs = devShell;
      };
    }))
    // {
      lib.mkFoundryPackage = {pkgs, ...} @ args:
        import ./nix/package.nix {
          lib = nixpkgs.lib;
          inherit pkgs;
        } (builtins.removeAttrs args ["pkgs"]);
      nixosModules.default = import ./nix/module.nix;
      # Composition surface for a consumer that wants the upstream native
      # Foundry service plus the companion broker. Canix supplies the concrete
      # package, host policy, credentials, and route values.
      nixosModules.foundry-stack = {
        imports = [
          nix-foundryvtt.nixosModules.foundryvtt
          (import ./nix/addons.nix)
          (import ./nix/module.nix)
        ];
      };
    };
}
