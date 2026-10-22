{
  perSystem =
    { lib, pkgs, ... }:
    let
      inherit (pkgs.stdenv.hostPlatform) isLinux isDarwin;
      hostTarget = pkgs.stdenv.hostPlatform.rust.cargoEnvVarTarget;
    in
    {
      devShells.default = pkgs.mkShell {
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
            tesseractBase = pkgs.tesseract5.passthru.tesseractBase.overrideAttrs (
              import ../ci/tesseract-build.nix
            );
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
          TESSDATA_PREFIX = "${import ../ci/tessdata.nix { inherit pkgs; }}";
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

      formatter = pkgs.nixfmt;
    };
}
