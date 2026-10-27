{
  description = "Foundry Circle: typed Foundry VTT broker and Dioxus operator console";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rs-harbor = {
      url = "git+https://github.com/caniko/harbor-rs.git?ref=trunk&rev=088186303fc68c839c047549c471874654e07bd4";
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
    nix-provenance.url = "git+ssh://git@github.com/caniko/nix-provenance.git";
  };

  outputs = {
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
        toolchainProfile = "nightly";
        extensions = ["rustfmt" "clippy" "rust-src"];
      };
      wasmToolchain = rs-harbor.lib.mkWasmToolchain {inherit pkgs;};
      sccachePackage = rs-harbor.packages.${system}.sccache;
      buildCache = rs-harbor.lib.mkBuildCachePolicy {
        inherit pkgs sccachePackage;
        buildPackageSet = pkgs.buildPackages;
        # Use Atlas' shared Redis/Valkey transport; /tmp is private to each
        # Nix sandbox and prevents cross-project compiler hits.
        cacheRoot = null;
        # Hosted CI has no Atlas transport; use a private per-build cache there.
        ephemeralFallback = true;
        namespaceScope = "canix-rust";
        namespaceGeneration = 5;
      };
      cacheRust = package: buildCache.withRustCache {inherit package;};
      cacheDioxus = package: buildCache.withDioxusCache {inherit package;};
      src = toolchain.craneLib.cleanCargoSource ./.;
      commonArgs = {
        inherit src;
        pname = "foundry-circle";
        strictDeps = true;
      };
      cargoArtifacts = cacheRust (toolchain.craneLib.buildDepsOnly commonArgs);
      cargoPackage = cacheRust (toolchain.craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--workspace --all-features";
        }));
      fetchPackage = cacheRust (toolchain.craneLib.buildPackage (commonArgs
        // {
          pname = "foundryvtt-fetch";
          cargoExtraArgs = "-p foundryvtt-fetch --bins";
        }));
      releaseBundle =
        pkgs.runCommand "foundry-circle-release-bundle" {
          nativeBuildInputs = [pkgs.gnutar pkgs.gzip];
        } ''
          mkdir -p staging/bin
          cp ${dioxusPackage}/bin/foundry-circle staging/bin/
          cp ${fetchPackage}/bin/foundryvtt-fetch staging/bin/
          cp ${fetchPackage}/bin/foundryvtt-fetchd staging/bin/
          cp ${fetchPackage}/bin/foundryvtt-fetch-hook staging/bin/
          tar -C staging -czf "$out" .
        '';
      dioxusPackage = cacheDioxus (rs-harbor.lib.mkDioxusFullstackPackage {
        inherit pkgs src;
        inherit (wasmToolchain) craneLib;
        inherit (wasmToolchain) rustToolchain;
        cargoLock = ./Cargo.lock;
        pname = "foundry-circle";
        package = "foundry-circle";
        binary = "foundry-circle";
        serverInstallName = "foundry-circle";
        # Dioxus fullstack emits the server artifact as `server`; the Cargo
        # binary remains foundry-circle and is selected above.
        serverBinary = "server";
        profile = "release";
        debugSymbols = false;
        noDefaultFeatures = true;
        webFeatures = ["web"];
        serverFeatures = ["server"];
        publicSubdir = "share/foundry-circle/public";
        wrapServer = false;
      });
      treefmtEval = treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix);
      preCommit = git-hooks.lib.${system}.run {
        src = ./.;
        hooks = import ./nix/pre-commit.nix {
          inherit pkgs;
          treefmtWrapper = treefmtEval.config.build.wrapper;
          inherit (toolchain) rustToolchain;
        };
      };
      packageFixtureSource = pkgs.runCommand "foundry-package-fixture-source" {} ''
        mkdir -p "$out/module"
        cat > "$out/module/module.json" <<'JSON'
        {"id":"phase-two-fixture","version":"1.2.3","download":"https://example.invalid/phase-two-fixture.zip","compatibility":{"minimum":"13","verified":"14"}}
        JSON
        printf 'fixture\n' > "$out/module/content.txt"
      '';
      packageFixture = import ./nix/package-output.nix {
        inherit (nixpkgs) lib;
        inherit pkgs;
        source = packageFixtureSource;
        kind = "module";
        id = "phase-two-fixture";
        version = "1.2.3";
        manifestName = "module.json";
        manifestUrl = "https://example.invalid/module.json";
        url = "https://example.invalid/phase-two-fixture.zip";
        hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        downloadUrl = "https://example.invalid/phase-two-fixture.zip";
        compatibility = {
          minimum = "13";
          verified = "14";
        };
      };
      publishedPackages = [dioxusPackage fetchPackage];
      devShell = toolchain.craneLib.devShell {
        checks = self.checks.${system};
        packages = with pkgs;
          [cargo-deny cargo-nextest cargo-audit jq pre-commit rust-analyzer]
          ++ preCommit.enabledPackages;
        inherit (preCommit) shellHook;
      };
    in {
      packages = {
        default = dioxusPackage;
        foundry-circle = dioxusPackage;
        foundry-circle-cli = cargoPackage;
        foundryvtt-fetch = fetchPackage;
        foundryvtt-fetchd = fetchPackage;
        foundryvtt-fetch-hook = fetchPackage;
        release-bundle = releaseBundle;
      };

      apps =
        {
          default = flake-utils.lib.mkApp {
            drv = dioxusPackage;
            exePath = "/bin/foundry-circle";
          };
        }
        // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          # Simit prebuild publication on trunk (secret ATTIC_TOKEN). Linux
          # outputs only: the packages canix consumes, pushed to the public
          # canix cache. Never add a path that reaches the licensed Foundry
          # package (see checks.published-closure-unlicensed).
          push-flake-inputs = rs-harbor.lib.mkAtticPush {
            inherit pkgs;
            flake = ".";
            paths = publishedPackages;
            adapter = rs-harbor.lib.mkAdapter {
              attic = {
                endpoint = "https://attic.candee.baby";
                cache = "canix";
                tokenEnvVar = "ATTIC_TOKEN";
              };
            };
          };
        };

      formatter = treefmtEval.config.build.wrapper;

      checks = {
        default = cargoPackage;
        # The public cache must never receive licensed Foundry VTT content.
        published-closure-unlicensed =
          pkgs.runCommand "published-closure-unlicensed" {
            closure = pkgs.closureInfo {rootPaths = publishedPackages;};
          } ''
            set -eu
            if grep -iE 'foundryvtt-[0-9]|foundryvtt.*\.zip|FoundryVTT-.*\.zip|-foundryvtt$' "$closure/store-paths"; then
              echo 'published closure reaches licensed Foundry VTT content' >&2
              exit 1
            fi
            touch "$out"
          '';
        formatting = treefmtEval.config.build.check self;
        fmt = toolchain.craneLib.cargoFmt {
          inherit src;
          pname = "foundry-circle";
        };
        doctests = toolchain.craneLib.cargoTest (commonArgs
          // {
            inherit cargoArtifacts;
            cargoTestExtraArgs = "--workspace --all-features --doc";
            preBuild = ''
              # The RSA advisory waiver is valid only while SQLx's MySQL
              # backend is absent from every enabled target's compiled graph.
              cargo tree --workspace --all-features --target all --locked \
                --prefix none --edges normal,build,dev > compiled-dependencies.txt
              if grep -Eq '^rsa v[0-9]' compiled-dependencies.txt; then
                echo "RSA is now compiled: remove and re-review RUSTSEC-2023-0071 waiver" >&2
                exit 1
              fi
            '';
          });
        clippy = cacheRust (toolchain.craneLib.cargoClippy (commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--workspace --all-targets --all-features -- --deny warnings";
          }));
        foundry-package-fixture = packageFixture;
        foundry-module = import ./nix/tests/module.nix {
          inherit nixpkgs pkgs;
        };
        foundry-reconcile-vm = import ./nix/tests/reconcile.nix {
          inherit pkgs;
          foundryvttFetch = fetchPackage;
        };
      };

      devShells = {
        default = devShell;
        msrv = pkgs.mkShell {
          inputsFrom = [(devShell.overrideAttrs (_: {shellHook = "";}))];
          packages = [pkgs.rust-bin.stable."1.88.0".minimal];
          RUSTFLAGS = "";
          CARGO_ENCODED_RUSTFLAGS = "";
          RUSTC_WRAPPER = "";
        };
        # Simit's generated publish workflow uses this named shell for the
        # crate documentation gate.
        docs = devShell;
      };
    }))
    // {
      lib.mkFoundryPackage = {pkgs, ...} @ args:
        import ./nix/package.nix {
          inherit (nixpkgs) lib;
          inherit pkgs;
        } (builtins.removeAttrs args ["pkgs"]);
      nixosModules.default = import ./nix/module.nix;
      # Composition surface for a consumer that wants the upstream native
      # Foundry service plus the companion broker. Canix supplies the concrete
      # package, host policy, credentials, and route values.
      nixosModules.foundry-stack = {
        _module.args.nixFoundryvtt = nix-foundryvtt;
        imports = [
          nix-foundryvtt.nixosModules.foundryvtt
          (import ./nix/addons.nix)
          (import ./nix/acquisition.nix)
          (import ./nix/module.nix)
        ];
      };
    };
}
