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

      # ---- Release 体积优化（2060 KB 地板）：机器特定配置动态注入 ----
      # 依赖 Apple CommandLineTools 的 clang/ld64（机器特定路径只存在于
      # aarch64-darwin），由 devShell 环境注入而非写死在 .cargo/config.toml。
      # 非 aarch64-darwin 一律不设置；脱离 devShell 的普通 `cargo build --release`
      # 仍可用（只是不做体积优化）。
      #
      # 注意：nixpkgs cc-wrapper 的 setup-hook 会无条件 `export CC=clang
      # CXX=clang++`（覆盖 mkShell 顶层 env 变量），因此 CC/CXX 必须在 shellHook
      # 里最后 export——shellHook 在 setup-hook 之后运行，且 `nix develop --command`
      # 同样执行，保证 `cargo build --release` 拿到 Apple clang。
      sizeOptShellHook = if system == "aarch64-darwin" then ''
        export CC="${./.cargo/cc-wrap.sh}"
        export CXX="/Library/Developer/CommandLineTools/usr/bin/clang++"
      '' else "";
      # 不被 setup-hook 覆盖的变量直接作为 env 注入（每次 `nix develop` 都有，
      # 包括 `--command`，不依赖 shellHook）。
      sizeOptEnv = if system == "aarch64-darwin" then {
        CFLAGS = "-Oz -fvisibility=hidden -flto -fno-stack-check";
        # cargo 的 target linker 环境变量形式，等价于旧 .cargo/config 的 linker =
        CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER = "/Library/Developer/CommandLineTools/usr/bin/ld";
        # Apple ld64 不搜索 nix store 路径；build.rs 据此输出
        # `rustc-link-arg=-L<dir>`（对所有链接 target 生效）。引用 pkgs.libiconv
        # 使其进入 devShell 闭包，`nix store gc` 不会回收。
        MYAGENT_LIBICONV_LIB_DIR = "${pkgs.libiconv}/lib";
        # aws-lc-sys 等依赖的 build script 二进制也用 Apple ld64 链接且带 -liconv，
        # myagent 自己的 build.rs 无法影响它们的链接，因此用 target 级 RUSTFLAGS
        # 环境变量把 libiconv 搜索路径注入所有 rustc 链接。此 env 与
        # .cargo/config.toml 的 [target.aarch64-apple-darwin] rustflags（机器
        # outliner）是**合并**关系，不会互相覆盖（已验证）；store 路径由 flake
        # 动态解析，不写死任何哈希。
        CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS = "-C link-arg=-L${pkgs.libiconv}/lib";
      } else {};
    in {
      default = pkgs.mkShell ({
        packages = with pkgs; [rustToolchain];
        RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        # 关闭 rust-analyzer 内部 telemetry（减少 IPC 抖动）
        RA_TELEMETRY = "off";
        shellHook = ''
          ${sizeOptShellHook}
          # 仅交互式 tty 打印横幅；`nix develop --command`（stdout 被管道接管、非 tty）
          # 不打印，避免污染 exec 工具输出
          if [ -t 1 ]; then
            echo "🦀 $(rustc --version) + rust-analyzer $(rust-analyzer --version)"
          fi
        '';
      } // sizeOptEnv);
    });
  };
}
