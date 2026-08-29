{
  description = "Rust project — Hermes-fixed flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    # rust-overlay 提供最新 stable/nightly rust-analyzer
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {self, nixpkgs, rust-overlay}: let
    systems = ["aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux"];
    forEach = nixpkgs.lib.genAttrs systems;
  in {
    devShells = forEach (system: let
      overlays = [rust-overlay.overlays.default];
      pkgs = import nixpkgs { inherit system overlays; };
      # 锁到当前 stable（rust-overlay 拿最新的 stable toolchain）
      rustToolchain = pkgs.rust-bin.stable.latest.default.override {
        extensions = ["rust-src" "rust-analyzer" "clippy" "rustfmt"];
      };
    in {
      default = pkgs.mkShell {
        packages = with pkgs; [rustToolchain];
        RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        # 关闭 rust-analyzer 内部 telemetry（减少 IPC 抖动）
        RA_TELEMETRY = "off";
        shellHook = ''
          echo "🦀 $(rustc --version) + rust-analyzer $(rust-analyzer --version)"
        '';
      };
    });
  };
}
