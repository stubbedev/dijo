# The statically linked Linux executable, built with Crane: musl, tesseract,
# Leptonica, libheif and the image libraries they read with linked in (ci/static-toolchain.nix),
# the language data embedded (ci/ocr-static.nix). The third-party crates are built on their own
# (dependencies), from the manifests and lock file alone, so a change to the workspace's code
# rebuilds only its own crates and a binary cache serves the rest. The executable refers to no
# store path: tesseract's default language data directory, which it never reads, is blanked.
{
  nixpkgs,
  crane,
  system,
}:
let
  pkgs = nixpkgs.legacyPackages.${system};
  inherit (pkgs) lib;
  toolchain = import ./static-toolchain.nix { inherit nixpkgs system; };
  inherit (toolchain) cross;
  craneLib = crane.mkLib cross;
  root = ../.;
  members = (lib.importTOML (root + "/Cargo.toml")).workspace.members;
  src = lib.fileset.toSource {
    inherit root;
    fileset =
      lib.fileset.difference
        (lib.fileset.unions (
          [
            (root + "/Cargo.toml")
            (root + "/Cargo.lock")
            (root + "/LICENSE")
            (root + "/THIRD-PARTY-LICENSES.md")
          ]
          ++ map (member: root + "/${member}") members
        ))
        (root + "/extractous-cli/fuzz");
  };
  cargoExtraArgs = "--locked --package extractous --bin extractous --no-default-features --features static";
  common = {
    inherit src cargoExtraArgs;
    inherit (craneLib.crateNameFromCargoToml { cargoToml = root + "/extractous-cli/Cargo.toml"; })
      pname
      version
      ;
    strictDeps = true;
    doCheck = false;
    "CARGO_TARGET_${cross.stdenv.hostPlatform.rust.cargoEnvVarTarget}_RUSTFLAGS" =
      "-C target-feature=+crt-static";
  };
  dependencies = craneLib.buildDepsOnly (
    common
    // {
      pname = "${common.pname}-dependencies";
      buildPhaseCargoCommand = "cargoWithProfile build ${cargoExtraArgs}";
    }
  );
in
{
  inherit dependencies;
  executable = craneLib.buildPackage (
    common
    // {
      cargoArtifacts = dependencies;
      EXTRACTOUS_OCR_BUNDLE = import ./ocr-static.nix { inherit nixpkgs system; };
      EXTRACTOUS_STATIC_LIBS = lib.concatStringsSep ":" toolchain.staticLibs;
      EXTRACTOUS_STATIC_LICENSES = toolchain.decoderLicenses;
      nativeBuildInputs = [ pkgs.removeReferencesTo ];
      postInstall = ''
        remove-references-to -t ${toolchain.tesseract} $out/bin/extractous
      '';
      allowedReferences = [ ];
      meta.mainProgram = "extractous";
    }
  );
}
