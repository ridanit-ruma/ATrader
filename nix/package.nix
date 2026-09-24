{
  lib,
  rustPlatform,
  buildNpmPackage,
}:
let
  web = buildNpmPackage {
    pname = "atrader-web";
    version = "0.1.0";
    src = lib.fileset.toSource {
      root = ../web;
      fileset = lib.fileset.difference ../web (
        lib.fileset.unions [
          (lib.fileset.maybeMissing ../web/node_modules)
          (lib.fileset.maybeMissing ../web/dist)
        ]
      );
    };
    npmDepsHash = "sha256-UcAHFtpvpBGJ3NdcqmwYMu0NkiPZh9HV6vBRiWQe0XI=";
    installPhase = "cp -r dist $out";
  };
in
rustPlatform.buildRustPackage {
  pname = "atrader";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../migrations
      ../holidays.toml
      ../fees.toml
    ];
  };
  cargoLock = {
    lockFile = ../Cargo.lock;
    allowBuiltinFetchGit = true;
  };
  # rust-embed bakes the dashboard into the binary.
  preBuild = "mkdir -p web && cp -r ${web} web/dist";
  # The integration tests need Postgres and the network; `nix flake check` runs the VM test instead.
  doCheck = false;
  meta = {
    description = "Paper trading for Attacca agents, with a private dashboard";
    license = lib.licenses.agpl3Only;
    mainProgram = "atrader";
  };
}
