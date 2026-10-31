{
  description = "Paseo - self-hosted daemon for AI coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          paseo = pkgs.callPackage ./nix/package.nix {
            sourceSha = self.rev or "";
          };
        in
        {
          default = paseo;
          paseo = paseo;
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          packages = self.packages.${system};
          license = (builtins.fromJSON (builtins.readFile ./package.json)).license;
        in
        {
          license-metadata =
            assert packages.paseo.meta.license.spdxId == license;
            pkgs.runCommand "paseo-license-metadata" { } "touch $out";
          native-runtime = pkgs.runCommand "paseo-native-runtime" {
            nativeBuildInputs = [ pkgs.python3 ];
          } ''
            python ${./nix/check-runtime.py} ${packages.paseo} ${packages.paseo.version}
            touch "$out"
          '';
        } // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
          let
            service = (nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.paseo
                {
                  services.paseo = {
                    enable = true;
                    user = "alice";
                    relay.enable = false;
                    settings.daemon.mcp.enabled = true;
                  };
                }
              ];
            }).config.systemd.services.paseo;
          in {
            service-contract =
              assert service.serviceConfig.ExecStart == "${packages.paseo}/bin/paseo daemon run";
              assert service.serviceConfig.User == "alice";
              assert service.environment.PASEO_HOME == "/home/alice/.paseo";
              assert service.environment.PASEO_RELAY_ENABLED == "false";
              pkgs.runCommand "paseo-service-contract" { } "touch $out";
          }
        )
      );

      nixosModules.default = self.nixosModules.paseo;
      nixosModules.paseo =
        { pkgs, lib, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.paseo.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.paseo ];
            packages = [
              pkgs.rustc
              pkgs.cargo
              pkgs.rustfmt
              pkgs.rust-analyzer
              pkgs.git
              pkgs.python3
              # Build-time SDK/extension packaging only; not a daemon runtime.
              pkgs.nodejs_24
            ];
          };
        }
      );
    };
}
