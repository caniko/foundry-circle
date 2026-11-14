{
  pkgs,
  treefmtWrapper,
  rustToolchain ? null,
}: {
  treefmt = {
    enable = true;
    name = "treefmt";
    entry = "${treefmtWrapper}/bin/treefmt --fail-on-change";
    pass_filenames = false;
  };

  cargo-fmt = {
    enable = true;
    name = "cargo fmt";
    entry = "cargo fmt --all -- --check";
    extraPackages = pkgs.lib.optional (rustToolchain != null) rustToolchain;
    pass_filenames = false;
  };

  cargo-clippy = {
    enable = true;
    name = "cargo clippy";
    # The desktop window embeds tartan-ui's stylesheet while the console pulls
    # tartan-ui's `web` feature; in one `--all-features` resolve that pair is a
    # deliberate compile_error. `checks.clippy-desktop` lints the window with
    # its GTK inputs, so the shared hook excludes it.
    entry = "cargo clippy --workspace --all-targets --all-features --exclude foundry-circle-desktop -- --deny warnings";
    extraPackages = pkgs.lib.optional (rustToolchain != null) rustToolchain;
    pass_filenames = false;
  };

  cargo-msrv = {
    enable = true;
    name = "cargo check MSRV";
    entry = "${pkgs.rust-bin.stable."1.93.0".default}/bin/cargo check --workspace --all-features --exclude foundry-circle-desktop";
    extraPackages = [pkgs.rust-bin.stable."1.93.0".default];
    pass_filenames = false;
    stages = ["pre-push" "manual"];
  };

  cargo-audit = {
    enable = true;
    name = "cargo audit";
    entry = "cargo audit";
    extraPackages = pkgs.lib.optional (rustToolchain != null) rustToolchain ++ [pkgs.cargo-audit];
    pass_filenames = false;
  };

  cargo-deny = {
    enable = true;
    name = "cargo deny";
    entry = "cargo deny check bans licenses sources";
    extraPackages = pkgs.lib.optional (rustToolchain != null) rustToolchain ++ [pkgs.cargo-deny];
    pass_filenames = false;
  };

  nix-flake-check = {
    enable = true;
    name = "nix flake check";
    entry = "nix --extra-experimental-features 'nix-command flakes' flake check --cores 0 --max-jobs auto --no-update-lock-file";
    extraPackages = [pkgs.nix];
    pass_filenames = false;
    stages = ["manual"];
  };
}
