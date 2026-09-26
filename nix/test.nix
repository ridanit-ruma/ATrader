# Boots the module with a local Postgres and no Attacca link, and checks the dashboard answers.
self: {
  name = "atrader";
  nodes.machine = {
    imports = [ self.nixosModules.default ];
    services.atrader = {
      enable = true;
      zyris = false;
      credentials.DART_API_KEY = builtins.toFile "dart" "test-key";
    };
  };
  testScript = ''
    machine.wait_for_unit("atrader.service")
    machine.wait_for_open_port(8750)
    machine.succeed("curl -sf -o /dev/null -w '%{http_code}' http://127.0.0.1:8750/ | grep 200")
    machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8750/api/overview | grep 401")
    machine.succeed("atrader-manage account create bot Bot --cash KRW=1000000")
    machine.succeed("atrader-manage account list | grep bot")
    machine.succeed("journalctl -u atrader | grep 'fundamentals sources' | grep 'dart=true'")
  '';
}
