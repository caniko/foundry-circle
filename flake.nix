{
  description = "Foundry Circle: typed Foundry VTT broker and Dioxus operator console";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rs-harbor = {
      url = "git+https://github.com/caniko/harbor-rs.git?ref=trunk&rev=6055d7d2b3a05d7d91d50938c7fb6f862a5eec85";
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
      # Dioxus 0.8 needs a matching dx and wasm-bindgen CLI; nixpkgs has
      # neither, so both are built from crates.io (see nix/dioxus-pins.nix).
      dioxusPins = import ./nix/dioxus-pins.nix {inherit pkgs;};
      # harbor-rs ships native packages for the Linux systems it builds on;
      # other systems fall back to nixpkgs' sccache.
      sccachePackage = pkgs.lib.attrByPath ["packages" system "sccache"] pkgs.sccache rs-harbor;
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
      # What a wry/webkitgtk window needs on Linux. `dbus` is for libdbus-sys
      # at build time; `glib-networking` supplies the runtime TLS backends.
      # Other platforms get their own wiring with the release bundle.
      desktopBuildInputs =
        if pkgs.stdenv.hostPlatform.isLinux
        then
          with pkgs; [
            dbus
            glib-networking
            gtk3
            libsoup_3
            openssl
            webkitgtk_4_1
          ]
        else [];
      desktopNativeBuildInputs =
        if pkgs.stdenv.hostPlatform.isLinux
        then [pkgs.pkg-config]
        else [];
      src = toolchain.craneLib.cleanCargoSource ./.;
      commonArgs = {
        inherit src;
        pname = "foundry-circle";
        strictDeps = true;
      };
      foundryVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      # The shared artifact set is the headless one: the desktop window is
      # excluded here, exactly as the packages and checks that reuse it exclude
      # it, so this build never needs the webkitgtk stack.
      cargoArtifacts = cacheRust (toolchain.craneLib.buildDepsOnly (commonArgs
        // {
          cargoExtraArgs = "--locked --workspace --exclude foundry-circle-desktop";
        }));
      # The window links wry/webkitgtk, so its dependencies are built once, on
      # their own, with the GTK inputs available.
      desktopCargoArtifacts = cacheRust (toolchain.craneLib.buildDepsOnly (commonArgs
        // {
          pname = "foundry-circle-desktop";
          cargoExtraArgs = "--locked -p foundry-circle-desktop";
          nativeBuildInputs = desktopNativeBuildInputs;
          buildInputs = desktopBuildInputs;
        }));
      cargoPackage = cacheRust (toolchain.craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          # The desktop window is a GTK binary of its own; keep it out of the
          # headless CLI package and its dependency closure.
          cargoExtraArgs = "--workspace --all-features --exclude foundry-circle-desktop";
        }));
      fetchPackage = cacheRust (toolchain.craneLib.buildPackage (commonArgs
        // {
          pname = "foundryvtt-fetch";
          cargoExtraArgs = "-p foundryvtt-fetch --bins";
        }));
      uploadPackage = cacheRust (toolchain.craneLib.buildPackage (commonArgs
        // {
          pname = "foundry-circle-upload";
          cargoExtraArgs = "-p foundry-circle-upload --bins";
        }));
      # The window links wry/webkitgtk, so it is built on its own target gate
      # instead of inside the shared `--workspace` CLI build.
      desktopPackage = cacheRust (toolchain.craneLib.buildPackage (commonArgs
        // {
          pname = "foundry-circle-desktop";
          cargoExtraArgs = "-p foundry-circle-desktop --bins";
          cargoArtifacts = desktopCargoArtifacts;
          nativeBuildInputs =
            desktopNativeBuildInputs
            ++ pkgs.lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.wrapGAppsHook3;
          buildInputs = desktopBuildInputs;
        }));
      # The game-master tools ship on all three platforms; the server and the
      # three fetch binaries stay in the Linux archive. The server is not a
      # static ELF (it carries the web assets and its runtime), so nix-bundle
      # backs the Linux release contract.
      gmBins = ["foundry-circle-upload" "foundry-circle-desktop"];
      portableRelease =
        if system == "x86_64-linux"
        then
          rs-harbor.lib.mkPortableBinaryRelease {
            inherit pkgs;
            pname = "foundry-circle";
            version = foundryVersion;
            artifacts.x86_64-linux.entries = {
              foundry-circle.package = dioxusPackage;
              foundryvtt-fetch.package = fetchPackage;
              foundryvtt-fetchd.package = fetchPackage;
              foundryvtt-fetch-hook.package = fetchPackage;
              foundry-circle-upload.package = uploadPackage;
              foundry-circle-desktop.package = desktopPackage;
            };
          }
        else null;
      # ---- release archives -------------------------------------------------
      # One bundle per system: simit's release matrix builds
      # `packages.<system>.release-bundle` on that platform's runner and merges
      # the archives into the single signed release.
      linuxBins = ["foundry-circle" "foundryvtt-fetch" "foundryvtt-fetchd" "foundryvtt-fetch-hook"] ++ gmBins;
      linuxArchiveName = "foundry-circle-${foundryVersion}-x86_64-linux-nix-bundle.tar.gz";
      windowsArchiveName = "foundry-circle-${foundryVersion}-x86_64-windows.zip";
      darwinArchiveName = "foundry-circle-${foundryVersion}-aarch64-darwin.tar.gz";
      windowsExes = map (binary: "${binary}.exe") gmBins;
      # WebView2Loader.dll is the only non-system DLL the windows-gnu build
      # imports; WebView2 itself ships with Windows 10/11.
      windowsDlls = ["WebView2Loader.dll"];
      # The archive contract as data, so every system renders the same
      # `[package.metadata.binstall]` and `simit release verify` can prove each
      # requested platform from either runner. The publishable bundle stays
      # limited to the archives its own system can build.
      mkArchiveSpec = {
        name,
        system,
        format,
        binaries,
        rustTarget ? null,
        validation ? "none",
      }: {inherit name system format binaries rustTarget validation;};
      linuxArchiveSpec = mkArchiveSpec {
        name = linuxArchiveName;
        system = "x86_64-linux";
        format = "tar.gz";
        validation = "portable-executable";
        binaries = map (binary: "bin/${binary}") linuxBins;
      };
      windowsArchiveSpec = mkArchiveSpec {
        name = windowsArchiveName;
        system = "x86_64-windows";
        rustTarget = "x86_64-pc-windows-gnu";
        format = "zip";
        binaries = windowsExes;
      };
      darwinArchiveSpec = mkArchiveSpec {
        name = darwinArchiveName;
        system = "aarch64-darwin";
        rustTarget = "aarch64-apple-darwin";
        format = "tar.gz";
        binaries = gmBins;
      };
      binstallLib = import "${rs-harbor}/lib/binstall.nix" {inherit (pkgs) lib;};
      # `mkBinstallCheck` reads only descriptors, so the check carries no build
      # dependency on the archives themselves.
      binstallBundle = archives: {
        binstall = binstallLib.mkBinstallMetadata {
          version = foundryVersion;
          inherit archives;
        };
        binstallArchives = archives;
      };
      # Windows x86_64 cross builds. `mkCross.windowsEnv` supplies the MinGW
      # CC/AR/linker and Rust's target-specific flags.
      cross = rs-harbor.lib.mkCross {inherit pkgs system;};
      windowsPackage = crate: {
        pname = "${crate}-windows";
        cargoExtraArgs = "-p ${crate} --bins";
        CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";
        PKG_CONFIG_ALLOW_CROSS = "1";
        doCheck = false;
        nativeBuildInputs = [cross.mingwCC cross.mingwBinutils];
      };
      windowsUpload = toolchain.craneLib.buildPackage (commonArgs // cross.windowsEnv // windowsPackage "foundry-circle-upload");
      windowsDesktop = toolchain.craneLib.buildPackage (commonArgs
        // cross.windowsEnv
        // windowsPackage "foundry-circle-desktop"
        // {
          # Rust links WebView2Loader.dll as an import; ship the loader the
          # crate vendors so the app runs on a stock Windows.
          postInstall = ''
            loader="$(find -L "$cargoVendorDir" -path '*webview2-com-sys-*/x64/WebView2Loader.dll' -print -quit)"
            test -n "$loader"
            install -m0644 "$loader" "$out/bin/"
          '';
        });
      releaseArtifacts =
        if system == "x86_64-linux"
        then {
          linux = rs-harbor.lib.mkReleaseArtifact {
            inherit pkgs;
            pname = "foundry-circle";
            version = foundryVersion;
            name = linuxArchiveName;
            source = portableRelease.archives.x86_64-linux;
            sourcePath = linuxArchiveName;
            kind = "portable-binary-archive";
            format = "tar.gz";
            system = "x86_64-linux";
            validation = "portable-executable";
            consumable = true;
            binaries = map (binary: "bin/${binary}") linuxBins;
          };
          windows = rs-harbor.lib.mkReleaseArchive {
            inherit pkgs;
            pname = "foundry-circle";
            version = foundryVersion;
            name = windowsArchiveName;
            package = windowsDesktop;
            entries =
              pkgs.lib.mapAttrs' (binary: package: {
                name = "${binary}.exe";
                value = {
                  source = "${package}/bin/${binary}.exe";
                  mode = "0755";
                };
              }) {
                foundry-circle-upload = windowsUpload;
                foundry-circle-desktop = windowsDesktop;
              }
              // pkgs.lib.genAttrs windowsDlls (name: {
                source = "${windowsDesktop}/bin/${name}";
                mode = "0644";
              });
            kind = "binary";
            format = "zip";
            system = "x86_64-windows";
            rustTarget = "x86_64-pc-windows-gnu";
            consumable = true;
          };
        }
        else if system == "aarch64-darwin"
        then {
          darwin = rs-harbor.lib.mkReleaseArchive {
            inherit pkgs;
            pname = "foundry-circle";
            version = foundryVersion;
            name = darwinArchiveName;
            package = uploadPackage;
            entries = {
              foundry-circle-upload = {
                source = "${uploadPackage}/bin/foundry-circle-upload";
                mode = "0755";
              };
              foundry-circle-desktop = {
                source = "${desktopPackage}/bin/foundry-circle-desktop";
                mode = "0755";
              };
            };
            kind = "binary";
            format = "tar.gz";
            system = "aarch64-darwin";
            rustTarget = "aarch64-apple-darwin";
            consumable = true;
          };
        }
        else {};
      releaseBundle =
        if releaseArtifacts == {}
        then null
        else
          rs-harbor.lib.mkReleaseBundle {
            inherit pkgs;
            pname = "foundry-circle";
            version = foundryVersion;
            artifacts = releaseArtifacts;
          };
      dioxusPackage = cacheDioxus (rs-harbor.lib.mkDioxusFullstackPackage {
        inherit pkgs src;
        inherit (wasmToolchain) craneLib;
        inherit (wasmToolchain) rustToolchain;
        inherit (dioxusPins) dioxusCli wasmBindgenCli;
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
          ++ desktopNativeBuildInputs
          ++ preCommit.enabledPackages;
        # Building the workspace reaches the desktop crate, which needs the
        # webkitgtk stack to link.
        buildInputs = desktopBuildInputs;
        inherit (preCommit) shellHook;
      };
    in {
      packages =
        {
          default = dioxusPackage;
          foundry-circle = dioxusPackage;
          foundry-circle-cli = cargoPackage;
          foundry-circle-upload = uploadPackage;
          foundryvtt-fetch = fetchPackage;
          foundryvtt-fetchd = fetchPackage;
          foundryvtt-fetch-hook = fetchPackage;
        }
        // pkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.isLinux || pkgs.stdenv.hostPlatform.isDarwin) {
          # A webkitgtk window on Linux, WKWebView on macOS.
          foundry-circle-desktop = desktopPackage;
        }
        // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
          # MinGW cross builds; the release zip ships both.
          foundry-circle-upload-windows = windowsUpload;
          foundry-circle-desktop-windows = windowsDesktop;
        }
        // pkgs.lib.optionalAttrs (releaseBundle != null) {
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
            cargoTestExtraArgs = "--workspace --all-features --exclude foundry-circle-desktop --doc";
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
            # The desktop crate embeds tartan-ui's stylesheet and the console
            # pulls tartan-ui's `web` feature: in one resolve that pair is a
            # `compile_error`, so the GUI is linted by `clippy-desktop`.
            cargoClippyExtraArgs = "--workspace --all-targets --all-features --exclude foundry-circle-desktop -- --deny warnings";
          }));
        clippy-desktop = cacheRust (toolchain.craneLib.cargoClippy (commonArgs
          // {
            cargoArtifacts = desktopCargoArtifacts;
            cargoClippyExtraArgs = "-p foundry-circle-desktop --all-targets -- --deny warnings";
            nativeBuildInputs = desktopNativeBuildInputs;
            buildInputs = desktopBuildInputs;
          }));
        foundry-package-fixture = packageFixture;
        foundry-module = import ./nix/tests/module.nix {
          inherit nixpkgs pkgs;
        };
        foundry-reconcile-vm = import ./nix/tests/reconcile.nix {
          inherit pkgs;
          foundryvttFetch = fetchPackage;
        };
        # Fail until the publishable crate carries the exact
        # [package.metadata.binstall] block and every archive ships its bins.
        # foundryvtt-fetch is the only crates.io crate and ships in the Linux
        # archive only.
        binstall-metadata = rs-harbor.lib.mkBinstallCheck {
          inherit pkgs;
          bundle = binstallBundle [linuxArchiveSpec];
          cargoToml = ./crates/foundryvtt-fetch/Cargo.toml;
        };
        # The game-master tools ship on Linux, Windows and macOS; this check's
        # binstall targets are the platform contract simit release verify
        # proves from either release runner.
        binstall-metadata-foundry-circle-upload = rs-harbor.lib.mkBinstallCheck {
          inherit pkgs;
          bundle = binstallBundle [linuxArchiveSpec windowsArchiveSpec darwinArchiveSpec];
          cargoToml = ./crates/foundry-circle-upload/Cargo.toml;
        };
      };

      devShells = {
        default = devShell;
        msrv = pkgs.mkShell {
          inputsFrom = [(devShell.overrideAttrs (_: {shellHook = "";}))];
          packages = [pkgs.rust-bin.stable."1.93.0".minimal];
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
          (
            {
              lib,
              config,
              ...
            }: {
              # Foundry serves the uploaded assets itself, so the broker's
              # directory appears read-only at the data-relative prefix it
              # reports to clients (`services.foundryvtt.dataDir`/Data/<prefix>).
              config = lib.mkIf config.services.foundry-circle.assets.enable {
                systemd.services.foundryvtt.serviceConfig.BindReadOnlyPaths = [
                  "${config.services.foundry-circle.assets.directory}:${config.services.foundryvtt.dataDir}/Data/${config.services.foundry-circle.assets.prefix}"
                ];
                # `Data` is created explicitly: otherwise tmpfiles would make it
                # root-owned on a fresh dataDir and Foundry could not write there.
                systemd.tmpfiles.rules = [
                  "d ${config.services.foundryvtt.dataDir}/Data - foundryvtt foundryvtt"
                  "d ${config.services.foundryvtt.dataDir}/Data/${config.services.foundry-circle.assets.prefix} 0755 foundryvtt foundryvtt"
                ];
              };
            }
          )
        ];
      };
    };
}
