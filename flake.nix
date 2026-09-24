{
  description = "ATrader: paper trading for Attacca agents, with a private dashboard";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      packages.${system}.default = pkgs.callPackage ./nix/package.nix { };
      nixosModules.default = import ./nix/module.nix self;
      checks.${system}.vm = pkgs.testers.runNixOSTest (import ./nix/test.nix self);
    };
}
