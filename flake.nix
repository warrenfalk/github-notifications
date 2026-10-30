{
  description = "A local GitHub notification inbox and desktop notifier";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          package = pkgs.callPackage ./nix/package.nix { };
        in
        {
          default = package;
          github-notifications = package;
        }
      );
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/github-notifications";
          meta.description = self.packages.${system}.default.meta.description;
        };
      });
      checks = forAllSystems (system: {
        package = self.packages.${system}.default;
      });
      nixosModules.default = import ./nix/module.nix { inherit self; };
      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer
              pkg-config
              cmake
              sqlite
              gh
              libnotify
              xdg-utils
              nixfmt
            ];

            # Iced loads these libraries dynamically. Keep this list narrow;
            # graphics drivers continue to come from the host.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (self.packages.${system}.default.runtimeLibraries);
          };
        }
      );
    };
}
