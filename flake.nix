{
  description = "dijo's development shell and Nix-built release artifacts: the static Linux executable and its container image";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane/v0.24.0";
  };

  outputs =
    { nixpkgs, crane, ... }:
    let
      inherit (nixpkgs) lib;
    in
    {
      packages = lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          static = import ./ci/static-native.nix { inherit nixpkgs crane system; };
        in
        {
          default = static.executable;
          static = static.executable;
          static-dependencies = static.dependencies;
          ocr-bundle-linked = import ./ci/ocr-static.nix { inherit nixpkgs system; };
          container = import ./ci/container.nix {
            inherit pkgs;
            executable = lib.getExe static.executable;
          };
        }
      );

      devShells = lib.genAttrs [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ] (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs.stdenv.hostPlatform) isLinux isDarwin;
          hostTarget = pkgs.stdenv.hostPlatform.rust.cargoEnvVarTarget;
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
              pkgs.just
              pkgs.cargo-nextest
              pkgs.cargo-about
              pkgs.cargo-deny
              pkgs.cargo-cyclonedx
              pkgs.cargo-fuzz
              pkgs.pkg-config
              pkgs.openssl
              (pkgs.tesseract.override {
                enableLanguages = [ "eng" ];
                tesseractBase = pkgs.tesseract5.passthru.tesseractBase.overrideAttrs (old: {
                  patches = (old.patches or [ ]) ++ import ./ci/tesseract-patches.nix;
                });
              })
              pkgs.libheif.bin
              pkgs.git
              pkgs.jre_headless
              pkgs.gnumake
              pkgs.clang-tools
              pkgs.ccls
            ]
            ++ lib.optionals isLinux [
              pkgs.clang
              pkgs.mold
              pkgs.gdb
              pkgs.valgrind
            ]
            ++ lib.optionals isDarwin [
              pkgs.libiconv
              pkgs.lldb
            ];

            env = {
              RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
              OMP_THREAD_LIMIT = "1";
              TESSDATA_PREFIX = "${import ./ci/tessdata.nix { inherit pkgs; }}";
            }
            // lib.optionalAttrs isLinux {
              "CARGO_TARGET_${hostTarget}_LINKER" = lib.getExe pkgs.clang;
              "CARGO_TARGET_${hostTarget}_RUSTFLAGS" = "-C link-arg=-fuse-ld=mold";
            };

            # cargo looks up subcommands (cargo clippy, cargo fmt, ...) in $CARGO_HOME/bin before $PATH
            # unless that directory is on $PATH, so rustup proxies there would shadow this toolchain.
            shellHook = ''
              export PATH="$PATH:''${CARGO_HOME:-$HOME/.cargo}/bin"
            '';
          };
        }
      );
    };
}
