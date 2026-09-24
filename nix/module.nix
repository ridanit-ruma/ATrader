self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.atrader;
  inherit (lib) mkOption types;
  env = lib.optionalAttrs cfg.database.createLocally {
    DATABASE_URL = "postgres://atrader@localhost/atrader?host=/run/postgresql";
  } // {
    ATRADER_HTTP_ADDR = cfg.httpAddress;
    ATRADER_STATE_DIR = "/var/lib/atrader";
  } // cfg.environment;
  # Run the CLI as the service user against the service database, e.g. `atrader-manage user create ruma`.
  # Run the CLI as the service user against the service database, e.g. `atrader-manage user create ruma`.
  # A remote DATABASE_URL is read from its credential file here (as root) and handed over through
  # the environment, never on a command line.
  manage = pkgs.writeShellScriptBin "atrader-manage" ''
    ${lib.optionalString (cfg.credentials ? DATABASE_URL) ''export DATABASE_URL="$(cat ${lib.escapeShellArg cfg.credentials.DATABASE_URL})"''}
    exec ${pkgs.sudo}/bin/sudo --preserve-env=DATABASE_URL -u atrader ${lib.concatStringsSep " " (lib.mapAttrsToList (k: v: "${k}=${lib.escapeShellArg v}") env)} ${lib.getExe cfg.package} "$@"
  '';
in
{
  options.services.atrader = {
    enable = lib.mkEnableOption "ATrader";
    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "atrader.packages.\${system}.default";
    };
    zyris = mkOption {
      type = types.bool;
      default = true;
      description = "Connect to Attacca; needs the ZYRIS_CREDENTIAL credential. Off runs the simulator and dashboard only.";
    };
    httpAddress = mkOption {
      type = types.str;
      default = "127.0.0.1:8750";
      description = "Dashboard listen address. Keep it on loopback and publish it with `tailscaleServe`.";
    };
    credentials = mkOption {
      type = types.attrsOf types.path;
      default = { };
      example = {
        ZYRIS_CREDENTIAL = "/run/secrets/atrader-zyris";
        KIS_APP_KEY = "/run/secrets/kis-app-key";
        KIS_APP_SECRET = "/run/secrets/kis-app-secret";
        DART_API_KEY = "/run/secrets/dart-api-key";
      };
      description = "Secret files, passed with systemd LoadCredential and read through NAME_FILE. They never enter the Nix store or the environment.";
    };
    environment = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = {
        EDGAR_USER_AGENT = "ATrader you@example.com";
        RUST_LOG = "atrader=info";
      };
      description = "Extra non-secret environment variables.";
    };
    database = {
      createLocally = mkOption {
        type = types.bool;
        default = true;
        description = "Create a local Postgres database `atrader` owned by the `atrader` user (peer authentication). When off, pass the URL as `credentials.DATABASE_URL` so its password stays out of the Nix store.";
      };
    };
    tailscaleServe = mkOption {
      type = types.bool;
      default = false;
      description = "Publish the dashboard on this machine's tailnet name over HTTPS with `tailscale serve`. It owns port 443 of `tailscale serve`: stopping it turns that port's serve config off.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.database.createLocally || cfg.credentials ? DATABASE_URL;
        message = "services.atrader.credentials.DATABASE_URL is required when database.createLocally is off";
      }
      {
        assertion = !cfg.zyris || cfg.credentials ? ZYRIS_CREDENTIAL;
        message = "services.atrader.credentials.ZYRIS_CREDENTIAL is required unless zyris = false";
      }
    ];

    users.users.atrader = {
      isSystemUser = true;
      group = "atrader";
    };
    users.groups.atrader = { };

    services.postgresql = lib.mkIf cfg.database.createLocally {
      enable = true;
      ensureDatabases = [ "atrader" ];
      ensureUsers = [
        {
          name = "atrader";
          ensureDBOwnership = true;
        }
      ];
    };

    environment.systemPackages = [ manage ];

    systemd.services.atrader = {
      description = "ATrader";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ] ++ lib.optional cfg.database.createLocally "postgresql.service";
      requires = lib.optional cfg.database.createLocally "postgresql.service";
      environment = env // lib.mapAttrs' (name: _: lib.nameValuePair "${name}_FILE" "%d/${name}") cfg.credentials;
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} serve" + lib.optionalString (!cfg.zyris) " --no-zyris";
        LoadCredential = lib.mapAttrsToList (name: path: "${name}:${path}") cfg.credentials;
        User = "atrader";
        Group = "atrader";
        StateDirectory = "atrader";
        Restart = "on-failure";
        RestartSec = 5;
        # Hardening
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        CapabilityBoundingSet = "";
        UMask = "0077";
      };
    };

    systemd.services.atrader-tailscale-serve = lib.mkIf cfg.tailscaleServe {
      description = "Publish the ATrader dashboard on the tailnet";
      wantedBy = [ "multi-user.target" ];
      after = [
        "tailscaled.service"
        "atrader.service"
      ];
      wants = [ "tailscaled.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${lib.getExe config.services.tailscale.package} serve --bg --https=443 http://${cfg.httpAddress}";
        ExecStop = "${lib.getExe config.services.tailscale.package} serve --https=443 off";
      };
    };
  };
}
